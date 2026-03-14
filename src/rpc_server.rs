mod types;
mod db;
mod rpc_client;

use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use clap::Parser;
use dashmap::DashMap;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use types::{RpcRequest, RpcResponse, Transaction, TransactionReceipt};
use rpc_client::RpcClient;

// ---------------------------------------------------------------------------
// Thundering herd protection
//
// Problem: 1000 concurrent requests for the same uncached tx → 1000 upstream calls.
//
// Solution: InflightMap<key, broadcast::Sender<Result>>
//   - First request for a key: insert a Sender, do the upstream fetch, broadcast result.
//   - Subsequent requests: find existing Sender, subscribe, wait for the result.
//   - After broadcast: remove the key so the next miss creates a fresh entry.
//
// broadcast channel is used (vs oneshot) because we don't know how many waiters
// will show up before the fetch completes. Channel capacity = 1 since we send exactly once.
// ---------------------------------------------------------------------------
type InflightResult = Result<Option<Value>, String>;
type InflightMap = Arc<DashMap<String, broadcast::Sender<InflightResult>>>;

#[derive(Parser, Debug)]
#[command(name = "rise-rpc-server")]
struct Args {
    #[arg(long, env = "DB_PATH", default_value = "my_database.db")]
    db_path: String,

    #[arg(long, env = "RPC_HTTP", default_value = "https://testnet.riselabs.xyz")]
    rpc_http: String,

    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8545")]
    listen_addr: String,
}

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    rpc: Arc<RpcClient>,
    // One map per RPC method that can be cached
    inflight_tx:      InflightMap,
    inflight_receipt: InflightMap,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("rise_rpc_server=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    info!("Starting RISE RPC server | DB={} | upstream={} | listen={}", args.db_path, args.rpc_http, args.listen_addr);

    let pool = db::create_pool(&args.db_path).await?;
    let rpc = Arc::new(RpcClient::new(&args.rpc_http));

    let state = AppState {
        pool,
        rpc,
        inflight_tx:      Arc::new(DashMap::new()),
        inflight_receipt: Arc::new(DashMap::new()),
    };

    let app = Router::new()
        .route("/", post(handle_rpc))
        .route("/rpc", post(handle_rpc))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen_addr).await?;
    info!("RPC server ready");
    axum::serve(listener, app).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP handler — supports single and batch JSON-RPC
// ---------------------------------------------------------------------------
async fn handle_rpc(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let is_batch = body.first().map(|b| *b == b'[').unwrap_or(false);

    if is_batch {
        match serde_json::from_slice::<Vec<RpcRequest>>(&body) {
            Ok(requests) => {
                // Dispatch all requests concurrently within a batch
                let futs: Vec<_> = requests.into_iter()
                    .map(|req| {
                        let state = state.clone();
                        tokio::spawn(async move { dispatch(&state, req).await })
                    })
                    .collect();
                let mut responses = vec![];
                for f in futs {
                    match f.await {
                        Ok(r) => responses.push(r),
                        Err(e) => responses.push(RpcResponse::err(Value::Null, -32603, &e.to_string())),
                    }
                }
                Json(json!(responses)).into_response()
            }
            Err(e) => Json(RpcResponse::err(Value::Null, -32700, &format!("Parse error: {}", e))).into_response(),
        }
    } else {
        match serde_json::from_slice::<RpcRequest>(&body) {
            Ok(req) => Json(dispatch(&state, req).await).into_response(),
            Err(e) => (
                StatusCode::OK,
                Json(RpcResponse::err(Value::Null, -32700, &format!("Parse error: {}", e))),
            ).into_response(),
        }
    }
}

async fn dispatch(state: &AppState, req: RpcRequest) -> RpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        "eth_getTransactionByHash"  => handle_get_tx(state, req).await,
        "eth_getTransactionReceipt" => handle_get_receipt(state, req).await,
        other => proxy_to_upstream(state, other, req.params, id).await,
    }
}

// ---------------------------------------------------------------------------
// eth_getTransactionByHash — cache-first with thundering herd protection
// ---------------------------------------------------------------------------
async fn handle_get_tx(state: &AppState, req: RpcRequest) -> RpcResponse {
    let id = req.id.clone();
    let hash = match extract_hash_param(&req.params) {
        Ok(h) => h,
        Err(e) => return RpcResponse::err(id, -32602, &e),
    };

    // 1. SQLite cache
    match db::get_transaction(&state.pool, &hash).await {
        Ok(Some(tx)) => {
            info!("cache hit  tx {}", hash);
            return RpcResponse::ok(id, tx);
        }
        Ok(None) => {}
        Err(e) => warn!("db error tx {}: {:?}", hash, e),
    }

    // 2. Dedup in-flight upstream calls
    info!("cache miss tx {} → upstream", hash);
    match fetch_deduped(&state.inflight_tx, &hash, || {
        let rpc   = state.rpc.clone();
        let pool  = state.pool.clone();
        let hash  = hash.clone();
        async move {
            match rpc.get_transaction_by_hash(&hash).await {
                Ok(Some(tx)) => {
                    let raw = serde_json::to_string(&tx).unwrap_or_default();
                    if let Err(e) = db::insert_transaction(&pool, &tx, &raw).await {
                        warn!("index tx on-demand {}: {:?}", hash, e);
                    }
                    Ok(Some(serde_json::to_value(&tx).unwrap()))
                }
                Ok(None) => Ok(None),
                Err(e)   => Err(e.to_string()),
            }
        }
    }).await {
        Ok(Some(v)) => RpcResponse::ok(id, v),
        Ok(None)    => RpcResponse::ok(id, Value::Null),
        Err(e)      => RpcResponse::err(id, -32603, &format!("Upstream error: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// eth_getTransactionReceipt — cache-first with thundering herd protection
// ---------------------------------------------------------------------------
async fn handle_get_receipt(state: &AppState, req: RpcRequest) -> RpcResponse {
    let id = req.id.clone();
    let hash = match extract_hash_param(&req.params) {
        Ok(h) => h,
        Err(e) => return RpcResponse::err(id, -32602, &e),
    };

    // 1. SQLite cache
    match db::get_receipt(&state.pool, &hash).await {
        Ok(Some(r)) => {
            info!("cache hit  receipt {}", hash);
            return RpcResponse::ok(id, r);
        }
        Ok(None) => {}
        Err(e) => warn!("db error receipt {}: {:?}", hash, e),
    }

    // 2. Dedup in-flight upstream calls
    info!("cache miss receipt {} → upstream", hash);
    match fetch_deduped(&state.inflight_receipt, &hash, || {
        let rpc   = state.rpc.clone();
        let pool  = state.pool.clone();
        let hash  = hash.clone();
        async move {
            match rpc.get_transaction_receipt(&hash).await {
                Ok(Some(rcpt)) => {
                    let raw = serde_json::to_string(&rcpt).unwrap_or_default();
                    if let Err(e) = db::insert_receipt(&pool, &rcpt, &raw).await {
                        warn!("index receipt on-demand {}: {:?}", hash, e);
                    }
                    // Best-effort: also cache the tx while we're here
                    if db::get_transaction(&pool, &hash).await.ok().flatten().is_none() {
                        if let Ok(Some(tx)) = rpc.get_transaction_by_hash(&hash).await {
                            let raw = serde_json::to_string(&tx).unwrap_or_default();
                            let _ = db::insert_transaction(&pool, &tx, &raw).await;
                        }
                    }
                    Ok(Some(serde_json::to_value(&rcpt).unwrap()))
                }
                Ok(None) => Ok(None),
                Err(e)   => Err(e.to_string()),
            }
        }
    }).await {
        Ok(Some(v)) => RpcResponse::ok(id, v),
        Ok(None)    => RpcResponse::ok(id, Value::Null),
        Err(e)      => RpcResponse::err(id, -32603, &format!("Upstream error: {}", e)),
    }
}

// ---------------------------------------------------------------------------
// fetch_deduped — the thundering herd suppressor
//
// Flow for key K:
//
//  Request A (first):               Request B (concurrent miss):
//  ┌─ entry vacant                  ┌─ entry occupied → subscribe(rx)
//  │  insert Sender                 │
//  │  drop map lock                 │  await broadcast...
//  │  fetch upstream       ─────────┼─────────────────────────┐
//  │  send(result) to all           │                         │
//  │  remove entry          <───────┘  receive result         │
//  └─ return result                 └─ return result          │
//                                                             ▼
//                                              (entry removed, next miss
//                                               creates a fresh entry)
// ---------------------------------------------------------------------------
async fn fetch_deduped<F, Fut>(
    inflight: &InflightMap,
    key: &str,
    fetch_fn: F,
) -> InflightResult
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = InflightResult> + Send + 'static,
{
    // Check if there's already an in-flight request for this key
    if let Some(sender) = inflight.get(key) {
        // Subscribe before the lock is released so we don't miss the send
        let mut rx = sender.subscribe();
        drop(sender); // release DashMap read lock

        info!("thundering herd: waiting on in-flight request for {}", key);
        return match rx.recv().await {
            Ok(result) => result,
            // Sender dropped before sending (caller panicked) — treat as error
            Err(_) => Err("in-flight request was dropped".into()),
        };
    }

    // We're the first — create a channel and insert it
    // capacity=1: we send exactly once; lagged receivers get an error (impossible here
    // since we subscribe before sending, but capacity>0 is required)
    let (tx, _) = broadcast::channel(1);
    inflight.insert(key.to_string(), tx.clone());

    // Do the actual upstream fetch
    let result = fetch_fn().await;

    // Broadcast to all waiters then clean up
    // Ignore send errors — they just mean no one was waiting
    let _ = tx.send(result.clone());
    inflight.remove(key);

    result
}

// ---------------------------------------------------------------------------
// Generic proxy
// ---------------------------------------------------------------------------
async fn proxy_to_upstream(state: &AppState, method: &str, params: Value, id: Value) -> RpcResponse {
    match state.rpc.call(method, params).await {
        Ok(result) => RpcResponse::ok(id, result),
        Err(e)     => RpcResponse::err(id, -32603, &format!("Upstream error: {}", e)),
    }
}

fn extract_hash_param(params: &Value) -> Result<String, String> {
    params
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase())
        .ok_or_else(|| "Missing or invalid hash parameter".to_string())
}