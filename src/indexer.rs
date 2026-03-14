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

    #[arg(long, env = "RPC_WS", default_value = "wss://testnet.riselabs.xyz/ws")]
    rpc_ws: String,

    #[arg(long, default_value = "10")]
    backfill_batch: u64,

    #[arg(long)]
    no_realtime: bool,

    #[arg(long)]
    no_backfill: bool,
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

    if !args.no_realtime {
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
    }

    if !args.no_backfill {
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
    }

    for h in handles { let _ = h.await; }
    Ok(())
}

// ---------------------------------------------------------------------------
// Realtime: subscribe to RISE shreds
// ---------------------------------------------------------------------------
async fn realtime_indexer(pool: &SqlitePool, ws_url: &str) -> Result<()> {
    info!("Connecting to WS: {}", ws_url);
    let (ws_stream, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws_stream.split();

    write.send(Message::Text(json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "eth_subscribe",
        "params": ["shreds"]
    }).to_string())).await?;
    info!("Subscribed to shreds");

    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(text) => {
                let v: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => { warn!("WS parse error: {:?}", e); continue; }
                };

                if v.get("id").is_some() {
                    if let Some(sub_id) = v["result"].as_str() {
                        info!("Shred subscription confirmed: {}", sub_id);
                    }
                    continue;
                }

                if v["method"].as_str() != Some("eth_subscription") { continue; }
                let result = &v["params"]["result"];
                if result.get("shredIdx").is_none() { continue; }

                match serde_json::from_value::<Shred>(result.clone()) {
                    Ok(shred) => {
                        if let Err(e) = process_shred(pool, shred).await {
                            error!("process_shred error: {:?}", e);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to deserialize shred: {:?} | raw: {}", e, result);
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

async fn process_shred(pool: &SqlitePool, shred: Shred) -> Result<()> {
    if !shred.transactions.is_empty() {
        info!("Shred #{} block #{} — {} txs", shred.shred_idx, shred.block_number, shred.transactions.len());
    }

    let block_number_hex = format!("0x{:x}", shred.block_number);

    for shred_tx in &shred.transactions {
        let inner = &shred_tx.transaction;
        let rcpt  = &shred_tx.receipt;
        let hash  = &inner.hash;

        let tx = Transaction {
            hash: hash.clone(),
            block_hash: None,
            block_number: Some(block_number_hex.clone()),
            transaction_index: None,
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

        let gas_used = rcpt.gas_used.clone()
            .unwrap_or_else(|| rcpt.cumulative_gas_used.clone());

        let receipt = TransactionReceipt {
            transaction_hash: hash.clone(),
            block_hash: String::new(),
            block_number: block_number_hex.clone(),
            transaction_index: "0x0".into(),
            from: inner.signer.clone(),
            to: inner.to.clone(),
            contract_address: None,
            cumulative_gas_used: rcpt.cumulative_gas_used.clone(),
            gas_used,
            effective_gas_price: inner.gas_price.clone()
                .or_else(|| inner.max_fee_per_gas.clone()),
            status: rcpt.status.clone(),
            logs: rcpt.logs.clone(),
            logs_bloom: "0x".into(),
            tx_type: rcpt.tx_type.clone(),
        };
        let receipt_raw = serde_json::to_string(&receipt)?;
        if let Err(e) = db::insert_receipt(pool, &receipt, &receipt_raw).await {
            warn!("insert_receipt {}: {:?}", hash, e);
        }
    }

    db::set_latest_indexed_block(pool, shred.block_number).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Backfill: walk historical blocks using eth_getBlockReceipts
//
// Old approach (N+1):
//   eth_getBlockByNumber → N × eth_getTransactionReceipt
//   = 1 + N RPC calls per block
//
// New approach (2 calls flat):
//   eth_getBlockByNumber (txs)  ──┐
//   eth_getBlockReceipts        ──┘  join by tx hash
//   = always 2 RPC calls per block, regardless of tx count
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
    // Fire both requests concurrently — no reason to wait for one before the other
    let (block_res, receipts_res) = tokio::join!(
        rpc.get_block_by_number(block_number),
        rpc.get_block_receipts(block_number),
    );

    let block    = block_res?;
    let receipts = receipts_res?;   // Vec<Value>, one per tx

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

    info!("Backfill block #{}: {} txs, {} receipts", block_number, txs.len(), receipts.len());

    // Build a hash → receipt index so the join below is O(1) per tx
    let receipt_map: std::collections::HashMap<String, &Value> = receipts
        .iter()
        .filter_map(|r| {
            r["transactionHash"].as_str().map(|h| (h.to_lowercase(), r))
        })
        .collect();

    for tx_val in &txs {
        let tx: Transaction = match serde_json::from_value(tx_val.clone()) {
            Ok(t) => t,
            Err(e) => { warn!("parse tx: {:?}", e); continue; }
        };

        // Persist transaction (full data — block_hash, index, signatures all present)
        let tx_raw = tx_val.to_string();
        if let Err(e) = db::insert_transaction(pool, &tx, &tx_raw).await {
            warn!("insert tx {}: {:?}", tx.hash, e);
            continue;
        }

        // Join with receipt from the map — no extra RPC call
        match receipt_map.get(&tx.hash.to_lowercase()) {
            Some(rcpt_val) => {
                match serde_json::from_value::<TransactionReceipt>((*rcpt_val).clone()) {
                    Ok(rcpt) => {
                        let raw = rcpt_val.to_string();
                        if let Err(e) = db::insert_receipt(pool, &rcpt, &raw).await {
                            warn!("insert receipt {}: {:?}", tx.hash, e);
                        }
                    }
                    Err(e) => warn!("parse receipt {}: {:?}", tx.hash, e),
                }
            }
            None => warn!("no receipt in block for tx {}", tx.hash),
        }
    }

    db::set_latest_indexed_block(pool, block_number).await?;
    Ok(())
}