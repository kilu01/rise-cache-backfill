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
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use types::{RpcRequest, RpcResponse, Transaction, TransactionReceipt};
use rpc_client::RpcClient;

#[derive(Parser, Debug)]
#[command(name = "rise-rpc-server")]
struct Args {
    /// SQLite database path (shared with indexer)
    #[arg(long, env = "DB_PATH", default_value = "my_database.db")]
    db_path: String,

    /// RISE testnet HTTP RPC endpoint (upstream fallback)
    #[arg(long, env = "RPC_HTTP", default_value = "https://testnet.riselabs.xyz")]
    rpc_http: String,

    /// Listen address
    #[arg(long, env = "LISTEN_ADDR", default_value = "0.0.0.0:8545")]
    listen_addr: String,
}

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
    rpc: Arc<RpcClient>,
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
    info!("Starting RISE RPC server");
    info!("DB: {}", args.db_path);
    info!("Upstream: {}", args.rpc_http);
    info!("Listening on: {}", args.listen_addr);

    let pool = db::create_pool(&args.db_path).await?;
    let rpc = Arc::new(RpcClient::new(&args.rpc_http));

    let state = AppState { pool, rpc };

    let app = Router::new()
        .route("/", post(handle_rpc))
        .route("/rpc", post(handle_rpc))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.listen_addr).await?;
    info!("RPC server ready at http://{}", args.listen_addr);
    axum::serve(listener, app).await?;

    Ok(())
}

async fn handle_rpc(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    // Support both single request and batch
    let is_batch = body.first().map(|b| *b == b'[').unwrap_or(false);

    if is_batch {
        match serde_json::from_slice::<Vec<RpcRequest>>(&body) {
            Ok(requests) => {
                let mut responses = vec![];
                for req in requests {
                    let resp = dispatch(&state, req).await;
                    responses.push(resp);
                }
                Json(json!(responses)).into_response()
            }
            Err(e) => {
                let resp = RpcResponse::err(Value::Null, -32700, &format!("Parse error: {}", e));
                Json(resp).into_response()
            }
        }
    } else {
        match serde_json::from_slice::<RpcRequest>(&body) {
            Ok(req) => {
                let resp = dispatch(&state, req).await;
                Json(resp).into_response()
            }
            Err(e) => {
                let resp = RpcResponse::err(Value::Null, -32700, &format!("Parse error: {}", e));
                (StatusCode::OK, Json(resp)).into_response()
            }
        }
    }
}

async fn dispatch(state: &AppState, req: RpcRequest) -> RpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        "eth_getTransactionByHash" => handle_get_tx(state, req).await,
        "eth_getTransactionReceipt" => handle_get_receipt(state, req).await,
        other => {
            info!("Proxying method: {}", other);
            proxy_to_upstream(state, other, req.params, id).await
        }
    }
}

// ---- eth_getTransactionByHash ----
async fn handle_get_tx(state: &AppState, req: RpcRequest) -> RpcResponse {
    let id = req.id.clone();
    let hash = match extract_hash_param(&req.params) {
        Ok(h) => h,
        Err(e) => return RpcResponse::err(id, -32602, &e),
    };

    // 1. Try cache/DB
    match db::get_transaction(&state.pool, &hash).await {
        Ok(Some(tx)) => {
            info!("Cache hit: eth_getTransactionByHash {}", hash);
            return RpcResponse::ok(id, tx);
        }
        Ok(None) => {}
        Err(e) => warn!("DB error for tx {}: {:?}", hash, e),
    }

    // 2. Fallback to upstream + index
    info!("Cache miss: eth_getTransactionByHash {} -> upstream", hash);
    match state.rpc.get_transaction_by_hash(&hash).await {
        Ok(Some(tx)) => {
            let raw = serde_json::to_string(&tx).unwrap_or_default();
            if let Err(e) = db::insert_transaction(&state.pool, &tx, &raw).await {
                warn!("Failed to index tx on demand {}: {:?}", hash, e);
            }
            RpcResponse::ok(id, serde_json::to_value(&tx).unwrap())
        }
        Ok(None) => RpcResponse::ok(id, Value::Null),
        Err(e) => RpcResponse::err(id, -32603, &format!("Upstream error: {}", e)),
    }
}

// ---- eth_getTransactionReceipt ----
async fn handle_get_receipt(state: &AppState, req: RpcRequest) -> RpcResponse {
    let id = req.id.clone();
    let hash = match extract_hash_param(&req.params) {
        Ok(h) => h,
        Err(e) => return RpcResponse::err(id, -32602, &e),
    };

    // 1. Try cache/DB
    match db::get_receipt(&state.pool, &hash).await {
        Ok(Some(receipt)) => {
            info!("Cache hit: eth_getTransactionReceipt {}", hash);
            return RpcResponse::ok(id, receipt);
        }
        Ok(None) => {}
        Err(e) => warn!("DB error for receipt {}: {:?}", hash, e),
    }

    // 2. Fallback to upstream + index
    info!("Cache miss: eth_getTransactionReceipt {} -> upstream", hash);
    match state.rpc.get_transaction_receipt(&hash).await {
        Ok(Some(receipt)) => {
            let raw = serde_json::to_string(&receipt).unwrap_or_default();
            if let Err(e) = db::insert_receipt(&state.pool, &receipt, &raw).await {
                warn!("Failed to index receipt on demand {}: {:?}", hash, e);
            }
            // Also try to index the tx itself if not already present
            if db::get_transaction(&state.pool, &hash).await.ok().flatten().is_none() {
                if let Ok(Some(tx)) = state.rpc.get_transaction_by_hash(&hash).await {
                    let raw = serde_json::to_string(&tx).unwrap_or_default();
                    let _ = db::insert_transaction(&state.pool, &tx, &raw).await;
                }
            }
            RpcResponse::ok(id, serde_json::to_value(&receipt).unwrap())
        }
        Ok(None) => RpcResponse::ok(id, Value::Null),
        Err(e) => RpcResponse::err(id, -32603, &format!("Upstream error: {}", e)),
    }
}

// ---- Generic proxy to upstream ----
async fn proxy_to_upstream(
    state: &AppState,
    method: &str,
    params: Value,
    id: Value,
) -> RpcResponse {
    match state.rpc.call(method, params).await {
        Ok(result) => RpcResponse::ok(id, result),
        Err(e) => RpcResponse::err(id, -32603, &format!("Upstream error: {}", e)),
    }
}

// ---- Helpers ----
fn extract_hash_param(params: &Value) -> Result<String, String> {
    params
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase())
        .ok_or_else(|| "Missing or invalid hash parameter".to_string())
}