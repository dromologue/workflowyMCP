# Multi-stage build for the remote MCP connector (workflowy-mcp-http).
# The stdio binary (Claude Desktop) is not shipped in this image.

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin workflowy-mcp-http

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home connector
COPY --from=builder /build/target/release/workflowy-mcp-http /usr/local/bin/workflowy-mcp-http

# No persistent volume by default: the name index lives in memory
# (WORKFLOWY_INDEX_PATH unset). Mount a volume + set the path to enable
# persistence — see docs/REMOTE-CONNECTOR.md.
ENV PORT=8080
EXPOSE 8080
USER connector
ENTRYPOINT ["/usr/local/bin/workflowy-mcp-http"]
