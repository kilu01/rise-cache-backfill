# RISE Testnet Indexer & RPC Server (PoC)

A Rust PoC that indexes RISE Testnet transactions/receipts and serves them via an Ethereum JSON-RPC-compatible HTTP server.

## Architecture

```
                  ┌─────────────────────────────────────┐
                  │         RISE Testnet Node            │
                  │   https://testnet.riselabs.xyz       │
                  │   wss://testnet.riselabs.xyz/ws      │
                  └────────────┬──────────────┬──────────┘
                               │ WS Shred  │ HTTP fallback
                               ▼              ▼
          ┌──────────────┐    ┌──────────────────────────┐
          │   Indexer    │    │      RPC Server           │
          │              │    │  (cache-first, fallback)  │
          │ • Realtime   │    │                           │
          │   (newHeads) │    │  eth_getTransactionByHash │
          │ • Backfill   │    │  eth_getTransactionReceipt│
          │   (best-eff) │    │  + proxy all others       │
          └──────┬───────┘    └──────────────┬────────────┘
                 │                           │
                 └──────────┬────────────────┘
                            ▼
                  ┌─────────────────┐
                  │  SQLite (rise.db)│
                  │  • transactions  │
                  │  • receipts      │
                  │  • indexer_state │
                  └─────────────────┘
```

## Components

### `indexer` binary
- **Realtime**: Subscribes to `eth_subscribe newHeads` via WebSocket. For each new block, fetches all transactions + receipts and persists to SQLite.
- **Backfill**: Scans blocks from last indexed up to current head, best-effort (skips failures, continues).

### `rpc-server` binary
- Listens on port 8545 (configurable).
- `eth_getTransactionByHash`: SQLite lookup → upstream fallback + index on demand.
- `eth_getTransactionReceipt`: SQLite lookup → upstream fallback + index on demand.
- All other methods: transparent proxy to upstream.
- Supports JSON-RPC batch requests.

## Quick Start

### Build and Run (Docker Compose)

```bash
docker-compose up --build
```

### Build locally

```bash
# Requires Rust 1.76+ and libssl-dev
cargo build --release

# Run indexer
./target/release/indexer --db-path my_database.db --rpc-http https://testnet.riselabs.xyz --rpc-ws wss://testnet.riselabs.xyz/ws

# Run RPC server (separate terminal, same DB)
./target/release/rpc-server --db-path my_database.db --rpc-http https://testnet.riselabs.xyz --listen-addr 0.0.0.0:8545
```

### Test

```bash
# Get a transaction (cache miss first time, then cache hit)
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getTransactionByHash","params":["0x34396566d3d7014ecdde7264809f2aaa8cd0d6882904061934246edc86227447"],"id":1}'

# Get receipt
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getTransactionReceipt","params":["0xfbeb23cf51e96fc900f89707b588d2833a0a2a58e262eb37f5996d8d66fb3bf8"],"id":1}'

# Any other method - proxied to upstream
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Batch request
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '[
    {"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1},
    {"jsonrpc":"2.0","method":"eth_getTransactionByHash","params":["0xYOUR_TX_HASH"],"id":2}
  ]'
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `DB_PATH` | `rise.db` | SQLite database path |
| `RPC_HTTP` | `https://testnet.riselabs.xyz` | Upstream HTTP RPC |
| `RPC_WS` | `wss://testnet.riselabs.xyz/ws` | Upstream WebSocket RPC |
| `LISTEN_ADDR` | `0.0.0.0:8545` | RPC server listen address |
| `RUST_LOG` | `info` | Log level |

## CLI Flags (indexer)

```
--no-realtime    Disable WebSocket realtime indexing
--no-backfill    Disable historical backfill
--backfill-batch Number of blocks per backfill batch (default: 10)
```

## Database Schema

```sql
transactions  (hash PK, block_hash, block_number, from_addr, to_addr, ...)
receipts      (transaction_hash PK, block_number, gas_used, status, logs, ...)
indexer_state (key, value) -- tracks latest indexed block
```

## Notes / Out of Scope (PoC)

- **Reorg handling**: Not implemented. Reorgs would leave stale data.
- **Shred-specific API**: Using standard `eth_subscribe newHeads` — if RISE exposes a proprietary shred subscription endpoint, swap in `rpc_ws` accordingly.
- **WAL mode**: For production, enable SQLite WAL (`PRAGMA journal_mode=WAL`) to allow concurrent reads while the indexer writes.
- **Testing/benchmarking/monitoring**: Out of scope per requirements.
- **Use COALESCE when backfill to avoid overiding with NUL value
- **Batch insert tx from ws
- **Upstream Semaphore to limit different key request going upstream
- **Exponential backoff when retry Shred WS