mod types;
mod db;
mod rpc_client;

use anyhow::Result;
use clap::Parser;
use serde_json::Value;
use sqlx::SqlitePool;
use tracing::{error, info, warn};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::{SinkExt, StreamExt};
use serde_json::json;

use types::{Shred, Transaction, TransactionReceipt};
use rpc_client::RpcClient;

#[derive(Parser, Debug)]
#[command(name = "rise-indexer")]
struct Args {
    #[arg(long, env = "DB_PATH", default_value = "my_database.db")]
    db_path: String,

    #[arg(long, env = "RPC_HTTP", default_value = "https://testnet.riselabs.xyz")]
    rpc_http: String,

    /// RISE WebSocket endpoint — shreds subscription
    #[arg(long, env = "RPC_WS", default_value = "wss://testnet.riselabs.xyz/ws")]
    rpc_ws: String,

    #[arg(long, default_value = "10")]
    backfill_batch: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("rise_indexer=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    info!("Starting RISE indexer | DB={} | HTTP={} | WS={}", args.db_path, args.rpc_http, args.rpc_ws);

    let pool = db::create_pool(&args.db_path).await?;
    let rpc = RpcClient::new(&args.rpc_http);
    let mut handles = vec![];

    let pool_rt = pool.clone();
    let ws_url = args.rpc_ws.clone();
    handles.push(tokio::spawn(async move {
        loop {
            if let Err(e) = realtime_indexer(&pool_rt, &ws_url).await {
                error!("Realtime indexer crashed: {:?}. Reconnecting in 5s...", e);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }));

    let pool_bf = pool.clone();
    let rpc_bf = rpc.clone();
    let batch = args.backfill_batch;
    handles.push(tokio::spawn(async move {
        loop {
            if let Err(e) = backfill_indexer(&pool_bf, &rpc_bf, batch).await {
                error!("Backfill error: {:?}. Retrying in 10s...", e);
                sleep(Duration::from_secs(10)).await;
            }
            sleep(Duration::from_secs(30)).await;
        }
    }));

    for h in handles { let _ = h.await; }
    Ok(())
}

// ---------------------------------------------------------------------------
// Realtime: subscribe to "shreds" — RISE-specific sub delivering tx+receipt
// data in milliseconds, with no extra RPC calls needed per transaction.
// ---------------------------------------------------------------------------
async fn realtime_indexer(pool: &SqlitePool, ws_url: &str) -> Result<()> {
    info!("Connecting to WS: {}", ws_url);
    let (ws_stream, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Subscribe to RISE shreds. The optional `true` second param enables
    // stateChanges; we skip it since we only need tx/receipt data.
    let sub_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["shreds"]
    });
    write.send(Message::Text(sub_req.to_string())).await?;
    info!("Subscribed to shreds");

    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => {
                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => { warn!("WS parse error: {:?}", e); continue; }
                };

                // {"jsonrpc":"2.0","id":1,"result":"0x9ce59a..."}
                if v.get("id").is_some() {
                    if let Some(sub_id) = v["result"].as_str() {
                        info!("Shred subscription confirmed: {}", sub_id);
                    }
                    continue;
                }

                // {"jsonrpc":"2.0","method":"eth_subscription","params":{...}}
                if v["method"].as_str() != Some("eth_subscription") { continue; }

                let result = &v["params"]["result"];

                // Distinguish shred notifications from other sub types (logs, newHeads)
                // by the presence of the "shredIdx" field.
                if result.get("shredIdx").is_none() { continue; }

                match serde_json::from_value::<Shred>(result.clone()) {
                    Ok(shred) => {
                        if let Err(e) = process_shred(pool, shred).await {
                            error!("process_shred error: {:?}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to deserialize shred: {:?}", e);
                        warn!("Raw: {}", result);
                    }
                }
            }
            Message::Ping(data) => { write.send(Message::Pong(data)).await?; }
            Message::Close(_) => { warn!("WS closed by server"); break; }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Process one shred: persist all tx+receipts.
//
// Shreds contain BOTH transaction and receipt data inline — no extra RPC calls.
// Shreds DON'T contain: block_hash, transaction_index, logs_bloom, signatures.
// Those fields are left empty here and enriched by the backfill path.
// ---------------------------------------------------------------------------
async fn process_shred(pool: &SqlitePool, shred: Shred) -> Result<()> {
    if !shred.transactions.is_empty() {
        info!(
            "Shred #{} block #{} — {} txs",
            shred.shred_idx, shred.block_number, shred.transactions.len()
        );
    }

    let block_number_hex = format!("0x{:x}", shred.block_number);

    for shred_tx in &shred.transactions {
        let inner = &shred_tx.transaction;
        let rcpt  = &shred_tx.receipt;
        let hash  = &inner.hash;

        // Build Transaction
        let tx = Transaction {
            hash: hash.clone(),
            block_hash: None,                          // not in shred
            block_number: Some(block_number_hex.clone()),
            transaction_index: None,                   // not in shred
            from: inner.signer.clone(),
            to: inner.to.clone(),
            value: inner.value.clone(),
            gas: inner.gas.clone().unwrap_or_else(|| "0x0".into()),
            gas_price: inner.gas_price.clone(),
            max_fee_per_gas: inner.max_fee_per_gas.clone(),
            max_priority_fee_per_gas: inner.max_priority_fee_per_gas.clone(),
            input: inner.input.clone().unwrap_or_else(|| "0x".into()),
            nonce: inner.nonce.clone().unwrap_or_else(|| "0x0".into()),
            tx_type: inner.tx_type.clone(),
            chain_id: inner.chain_id.clone(),
            v: None, r: None, s: None,
        };
        let tx_raw = serde_json::to_string(&tx)?;
        if let Err(e) = db::insert_transaction(pool, &tx, &tx_raw).await {
            warn!("insert_transaction {}: {:?}", hash, e);
            continue;
        }

        // Build TransactionReceipt
        // gas_used: prefer the dedicated field; fall back to cumulative as best-effort.
        let gas_used = rcpt.gas_used.clone()
            .unwrap_or_else(|| rcpt.cumulative_gas_used.clone());

        let receipt = TransactionReceipt {
            transaction_hash: hash.clone(),
            block_hash: String::new(),                 // not in shred
            block_number: block_number_hex.clone(),
            transaction_index: "0x0".into(),           // not in shred
            from: inner.signer.clone(),
            to: inner.to.clone(),
            contract_address: None,
            cumulative_gas_used: rcpt.cumulative_gas_used.clone(),
            gas_used,
            effective_gas_price: inner.gas_price.clone()
                .or_else(|| inner.max_fee_per_gas.clone()),
            status: rcpt.status.clone(),
            logs: rcpt.logs.clone(),
            logs_bloom: "0x".into(),                   // not in shred
            tx_type: rcpt.tx_type.clone(),
        };
        let receipt_raw = serde_json::to_string(&receipt)?;
        if let Err(e) = db::insert_receipt(pool, &receipt, &receipt_raw).await {
            warn!("insert_receipt {}: {:?}", hash, e);
        }
    }

    // Idempotent — safe to call multiple times per block (multiple shreds/block)
    db::set_latest_indexed_block(pool, shred.block_number).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Backfill: walk historical blocks via HTTP RPC.
// Serves two purposes:
//   1. Fill gaps caused by WS downtime.
//   2. Enrich shred-indexed rows with block_hash, transaction_index, logs_bloom.
// ---------------------------------------------------------------------------
async fn backfill_indexer(pool: &SqlitePool, rpc: &RpcClient, batch_size: u64) -> Result<()> {
    let current_head = rpc.get_block_number().await?;
    let last_indexed = db::get_latest_indexed_block(pool).await?.unwrap_or(0);

    if last_indexed >= current_head {
        info!("Backfill up-to-date at block {}", current_head);
        return Ok(());
    }

    info!("Backfill: blocks {} → {}", last_indexed, current_head);

    let mut block = last_indexed;
    while block <= current_head {
        let end = (block + batch_size).min(current_head);
        for b in block..=end {
            if let Err(e) = index_block(pool, rpc, b).await {
                warn!("Backfill skip block {}: {:?}", b, e);
            }
        }
        block = end + 1;
        sleep(Duration::from_millis(100)).await;
    }

    info!("Backfill complete at block {}", current_head);
    Ok(())
}

async fn index_block(pool: &SqlitePool, rpc: &RpcClient, block_number: u64) -> Result<()> {
    let block = rpc.get_block_by_number(block_number).await?;
    if block.is_null() {
        warn!("Block {} not found", block_number);
        return Ok(());
    }

    let txs = match block.get("transactions") {
        Some(Value::Array(t)) => t.clone(),
        _ => vec![],
    };

    if txs.is_empty() {
        db::set_latest_indexed_block(pool, block_number).await?;
        return Ok(());
    }

    info!("Backfill block #{}: {} txs", block_number, txs.len());

    for tx_val in &txs {
        let tx: Transaction = match serde_json::from_value(tx_val.clone()) {
            Ok(t) => t,
            Err(e) => { warn!("parse tx failed: {:?}", e); continue; }
        };

        let tx_raw = tx_val.to_string();
        if let Err(e) = db::insert_transaction(pool, &tx, &tx_raw).await {
            warn!("insert tx {}: {:?}", tx.hash, e);
            continue;
        }

        match rpc.get_transaction_receipt(&tx.hash).await {
            Ok(Some(rcpt)) => {
                let raw = serde_json::to_string(&rcpt)?;
                if let Err(e) = db::insert_receipt(pool, &rcpt, &raw).await {
                    warn!("insert receipt {}: {:?}", tx.hash, e);
                }
            }
            Ok(None) => warn!("no receipt for {}", tx.hash),
            Err(e)   => warn!("fetch receipt {}: {:?}", tx.hash, e),
        }
    }

    db::set_latest_indexed_block(pool, block_number).await?;
    Ok(())
}