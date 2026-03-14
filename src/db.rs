use anyhow::Result;
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};
use tracing::info;

use crate::types::{Transaction, TransactionReceipt};

pub async fn create_pool(db_path: &str) -> Result<SqlitePool> {
    let url = format!("sqlite://{}?mode=rwc", db_path);
    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect(&url)
        .await?;
    // run_migrations(&pool).await?;
    Ok(pool)
}

// async fn run_migrations(pool: &SqlitePool) -> Result<()> {
//     sqlx::query(include_str!("../migrations/001_init.sql"))
//         .execute(pool)
//         .await?;
//     info!("Migrations applied");
//     Ok(())
// }

// ---- Transaction ----

pub async fn insert_transaction(pool: &SqlitePool, tx: &Transaction, raw: &str) -> Result<()> {
    let block_number: Option<i64> = tx
        .block_number
        .as_deref()
        .and_then(|s| i64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let tx_index: Option<i64> = tx
        .transaction_index
        .as_deref()
        .and_then(|s| i64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let tx_type: i64 =
        i64::from_str_radix(tx.tx_type.trim_start_matches("0x"), 16).unwrap_or(0);

    sqlx::query!(
        r#"
        INSERT OR REPLACE INTO transactions (
            hash, block_hash, block_number, transaction_index,
            from_addr, to_addr, value, gas, gas_price,
            max_fee_per_gas, max_priority_fee_per_gas, input,
            nonce, type, chain_id, v, r, s, raw_json
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
            ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19
        )
        "#,
        tx.hash,
        tx.block_hash,
        block_number,
        tx_index,
        tx.from,
        tx.to,
        tx.value,
        tx.gas,
        tx.gas_price,
        tx.max_fee_per_gas,
        tx.max_priority_fee_per_gas,
        tx.input,
        tx.nonce,
        tx_type,
        tx.chain_id,
        tx.v,
        tx.r,
        tx.s,
        raw
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_transaction(pool: &SqlitePool, hash: &str) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query!("SELECT raw_json FROM transactions WHERE hash = ?1", hash)
        .fetch_optional(pool)
        .await?;

    if let Some(r) = row {
        let v: serde_json::Value = serde_json::from_str(&r.raw_json)?;
        return Ok(Some(v));
    }
    Ok(None)
}

// ---- Receipt ----

pub async fn insert_receipt(pool: &SqlitePool, r: &TransactionReceipt, raw: &str) -> Result<()> {
    let block_number: i64 =
        i64::from_str_radix(r.block_number.trim_start_matches("0x"), 16).unwrap_or(0);
    let tx_index: i64 =
        i64::from_str_radix(r.transaction_index.trim_start_matches("0x"), 16).unwrap_or(0);
    let tx_type: i64 =
        i64::from_str_radix(r.tx_type.trim_start_matches("0x"), 16).unwrap_or(0);
    let logs_str = r.logs.to_string();

    sqlx::query!(
        r#"
        INSERT OR REPLACE INTO receipts (
            transaction_hash, block_hash, block_number, transaction_index,
            from_addr, to_addr, contract_address, cumulative_gas_used,
            gas_used, effective_gas_price, status, logs, logs_bloom, type, raw_json
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12, ?13, ?14, ?15
        )
        "#,
        r.transaction_hash,
        r.block_hash,
        block_number,
        tx_index,
        r.from,
        r.to,
        r.contract_address,
        r.cumulative_gas_used,
        r.gas_used,
        r.effective_gas_price,
        r.status,
        logs_str,
        r.logs_bloom,
        tx_type,
        raw
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_receipt(pool: &SqlitePool, hash: &str) -> Result<Option<serde_json::Value>> {
    let row = sqlx::query!("SELECT raw_json FROM receipts WHERE transaction_hash = ?1", hash)
        .fetch_optional(pool)
        .await?;

    if let Some(r) = row {
        let v: serde_json::Value = serde_json::from_str(&r.raw_json)?;
        return Ok(Some(v));
    }
    Ok(None)
}

// ---- Indexer state ----

pub async fn get_state(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row = sqlx::query!("SELECT value FROM indexer_state WHERE key = ?1", key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.value))
}

pub async fn set_state(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query!(
        "INSERT OR REPLACE INTO indexer_state (key, value) VALUES (?1, ?2)",
        key,
        value
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_latest_indexed_block(pool: &SqlitePool) -> Result<Option<u64>> {
    let val = get_state(pool, "latest_block").await?;
    Ok(val.and_then(|v| v.parse::<u64>().ok()))
}

pub async fn set_latest_indexed_block(pool: &SqlitePool, block: u64) -> Result<()> {
    set_state(pool, "latest_block", &block.to_string()).await
}