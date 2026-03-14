FROM rust:1.76-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml .
COPY src/ src/
COPY migrations/ migrations/

RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/indexer /usr/local/bin/indexer
COPY --from=builder /app/target/release/rpc-server /usr/local/bin/rpc-server
COPY migrations/ /app/migrations/

RUN mkdir -p /data

ENTRYPOINT []
CMD ["rpc-server"]