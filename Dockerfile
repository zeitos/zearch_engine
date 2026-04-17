# ── Stage 1: build ──────────────────────────────────────────────────────────
FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    pkg-config \
    libssl-dev \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache dependency compilation separately from application code
COPY Cargo.toml Cargo.lock ./
COPY crates/core/Cargo.toml        crates/core/Cargo.toml
COPY crates/proto/Cargo.toml       crates/proto/Cargo.toml
COPY crates/analysis/Cargo.toml    crates/analysis/Cargo.toml
COPY crates/index/Cargo.toml       crates/index/Cargo.toml
COPY crates/query/Cargo.toml       crates/query/Cargo.toml
COPY crates/shard/Cargo.toml       crates/shard/Cargo.toml
COPY crates/ranker/Cargo.toml      crates/ranker/Cargo.toml
COPY crates/router/Cargo.toml      crates/router/Cargo.toml
COPY crates/server/Cargo.toml      crates/server/Cargo.toml

# Stub out every lib/main so Cargo can resolve and cache all deps
RUN for crate in core proto analysis index query shard ranker router; do \
      mkdir -p crates/$crate/src && echo "// stub" > crates/$crate/src/lib.rs; \
    done && \
    mkdir -p crates/server/src && echo "fn main() {}" > crates/server/src/main.rs

# Copy proto definitions (needed by build.rs in search-proto)
COPY proto/ proto/
COPY crates/proto/build.rs crates/proto/build.rs

RUN cargo build --release 2>/dev/null || true

# Now copy real source and rebuild only what changed
COPY crates/ crates/

RUN touch crates/*/src/*.rs crates/*/src/**/*.rs 2>/dev/null || true && \
    cargo build --release --bin search-engine

# Strip the binary
RUN strip /build/target/release/search-engine

# ── Stage 2: runtime ────────────────────────────────────────────────────────
FROM gcr.io/distroless/cc-debian12

COPY --from=builder /build/target/release/search-engine /usr/local/bin/search-engine

EXPOSE 8080 9001

ENTRYPOINT ["/usr/local/bin/search-engine"]
