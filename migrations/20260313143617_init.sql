-- Add migration script here

CREATE TABLE IF NOT EXISTS transactions (
    hash TEXT PRIMARY KEY,
    block_hash TEXT,
    block_number INTEGER,
    transaction_index INTEGER,
    from_addr TEXT NOT NULL,
    to_addr TEXT,
    value TEXT NOT NULL,
    gas TEXT NOT NULL,
    gas_price TEXT,
    max_fee_per_gas TEXT,
    max_priority_fee_per_gas TEXT,
    input TEXT NOT NULL,
    nonce TEXT NOT NULL,
    type INTEGER NOT NULL DEFAULT 0,
    chain_id TEXT,
    v TEXT,
    r TEXT,
    s TEXT,
    raw_json TEXT NOT NULL,
    indexed_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS receipts (
    transaction_hash TEXT PRIMARY KEY,
    block_hash TEXT NOT NULL,
    block_number INTEGER NOT NULL,
    transaction_index INTEGER NOT NULL,
    from_addr TEXT NOT NULL,
    to_addr TEXT,
    contract_address TEXT,
    cumulative_gas_used TEXT NOT NULL,
    gas_used TEXT NOT NULL,
    effective_gas_price TEXT,
    status TEXT NOT NULL,
    logs TEXT NOT NULL,
    logs_bloom TEXT NOT NULL,
    type INTEGER NOT NULL DEFAULT 0,
    raw_json TEXT NOT NULL,
    indexed_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS indexer_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_txs_block_number ON transactions(block_number);
CREATE INDEX IF NOT EXISTS idx_txs_from ON transactions(from_addr);
CREATE INDEX IF NOT EXISTS idx_txs_to ON transactions(to_addr);
CREATE INDEX IF NOT EXISTS idx_receipts_block_number ON receipts(block_number);