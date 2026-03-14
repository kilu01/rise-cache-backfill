use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---- Ethereum JSON-RPC types ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
    pub id: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: Value,
}

impl RpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), result: Some(result), error: None, id }
    }
    pub fn err(id: Value, code: i64, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(RpcError { code, message: message.to_string() }),
            id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

// ---- Ethereum Transaction ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_index: Option<String>,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub value: String,
    pub gas: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fee_per_gas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_priority_fee_per_gas: Option<String>,
    pub input: String,
    pub nonce: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
}

// ---- Ethereum Receipt ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionReceipt {
    pub transaction_hash: String,
    pub block_hash: String,
    pub block_number: String,
    pub transaction_index: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_address: Option<String>,
    pub cumulative_gas_used: String,
    pub gas_used: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_gas_price: Option<String>,
    pub status: String,
    pub logs: Value,
    pub logs_bloom: String,
    #[serde(rename = "type")]
    pub tx_type: String,
}


/// A RISE shred notification result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shred {
    pub block_timestamp: u64,
    pub block_number: u64,
    pub shred_idx: u64,
    pub starting_log_index: u64,
    pub transactions: Vec<ShredTransaction>,
    #[serde(default)]
    pub state_changes: serde_json::Value,
}

/// A transaction inside a shred (contains both tx fields + receipt fields inline)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShredTransaction {
    pub transaction: ShredTxInner,
    pub receipt: ShredReceiptInner,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShredTxInner {
    pub hash: String,
    /// "signer" is the from address in RISE shreds
    pub signer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub value: String,
    #[serde(rename = "type")]
    pub tx_type: String,
    // Optional fields depending on tx type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fee_per_gas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_priority_fee_per_gas: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShredReceiptInner {
    pub status: String,
    pub cumulative_gas_used: String,
    pub logs: serde_json::Value,
    #[serde(rename = "type")]
    pub tx_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_used: Option<String>,
}