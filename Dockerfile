# syntax=docker/dockerfile:1
# ---- Build the Rust binary (cargo-chef gives cached dependency layers) ----
FROM rust:1.99.0-bookworm AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies first - this layer is cached until Cargo.toml/lock change.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin proxmox-mcp-rs

# ---- Runtime: wrap the stdio MCP binary in supergateway -> streamable HTTP ----
# The upstream image ships a pure-stdio binary on distroless. For the shared
# mcp-shared stack we instead expose streamable HTTP on /mcp (port 8000), the
# same convention every other service in that stack uses (see mcp-portainer).
# supergateway spawns the binary and bridges stdio <-> streamable HTTP; the
# container environment (PROXMOX_*) is inherited by the child process.
FROM node:20.20.2-bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g supergateway@3.4.3 \
    && npm cache clean --force
WORKDIR /app
COPY --from=builder /app/target/release/proxmox-mcp-rs /app/proxmox-mcp-rs

# Force stdio so supergateway owns the transport (not the binary's native SSE).
ENV PROXMOX_SERVER_TYPE=stdio
EXPOSE 8000

ENTRYPOINT ["sh", "-c", "exec supergateway --outputTransport streamableHttp --port 8000 --stdio /app/proxmox-mcp-rs"]
