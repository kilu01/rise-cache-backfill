use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::types::{Transaction, TransactionReceipt};

#[derive(Clone)]
pub struct RpcClient {
    pub client: Client,
    pub endpoint: String,
}

impl RpcClient {
    pub fn new(endpoint: &str) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap(),
            endpoint: endpoint.to_string(),
        }
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1
        });
        debug!("RPC {} {:?}", method, params);

        let resp: Value = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if let Some(err) = resp.get("error") {
            return Err(anyhow!("RPC error: {}", err));
        }
        Ok(resp["result"].clone())
    }

    pub async fn get_transaction_by_hash(&self, hash: &str) -> Result<Option<Transaction>> {
        let result = self.call("eth_getTransactionByHash", json!([hash])).await?;
        if result.is_null() { return Ok(None); }
        Ok(Some(serde_json::from_value(result)?))
    }

    pub async fn get_transaction_receipt(&self, hash: &str) -> Result<Option<TransactionReceipt>> {
        let result = self.call("eth_getTransactionReceipt", json!([hash])).await?;
        if result.is_null() { return Ok(None); }
        Ok(Some(serde_json::from_value(result)?))
    }

    pub async fn get_block_number(&self) -> Result<u64> {
        let result = self.call("eth_blockNumber", json!([])).await?;
        let hex = result.as_str().ok_or_else(|| anyhow!("expected string"))?;
        Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16)?)
    }

    pub async fn get_block_by_number(&self, block: u64) -> Result<Value> {
        let hex = format!("0x{:x}", block);
        self.call("eth_getBlockByNumber", json!([hex, true])).await
    }

    /// Fetch all receipts for a block in ONE call — O(1) vs O(n_tx) per block.
    /// Returns a Vec of raw receipt Values (preserves upstream format for raw_json storage).
    pub async fn get_block_receipts(&self, block: u64) -> Result<Vec<Value>> {
        let hex = format!("0x{:x}", block);
        let result = self.call("eth_getBlockReceipts", json!([hex])).await?;
        match result {
            Value::Array(receipts) => Ok(receipts),
            Value::Null => Ok(vec![]),
            other => Err(anyhow!("eth_getBlockReceipts unexpected response: {}", other)),
        }
    }
}