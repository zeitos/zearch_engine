# Implementation Tasks: Product Search Engine

## Overview

Implementation is organized into 7 phases, ordered so each phase builds on the previous one and produces a testable increment. Every phase ends with something that can be run and verified.

## Phase 1 — Project Skeleton and Core Types

Foundation: workspace setup, core types, protobuf definitions, and a binary that compiles and starts.

**Depends on:** Nothing

- [x] 1.1 Initialize Cargo workspace with all 9 crates (`core`, `proto`, `analysis`, `index`, `query`, `shard`, `ranker`, `router`, `server`)
- [x] 1.2 Define core types: `Document`, `Value`, `FieldConfig`, `FieldType`, `IndexSchema`, `AnalyzerType`, `Language` in `core`
- [x] 1.3 Define ranking types: `RankCandidate`, `RankedResult`, `SearchHit`, `SearchRequest`, `SearchResponse` in `core`
- [x] 1.4 Define core error types and `Result` alias in `core`
- [x] 1.5 Define configuration structs (router config, shard config, ranker config, standalone config) with YAML/env parsing in `core`
- [x] 1.6 Write `shard.proto` with all gRPC service definitions (`Search`, `GetDocs`, `Index`, `Delete`, `Stats`, `Health`)
- [x] 1.7 Write `ranker.proto` with `RankerService` definition (`Rerank` RPC)
- [x] 1.8 Set up `proto` crate with `build.rs` for tonic/prost code generation from both proto files
- [x] 1.9 Create `server` binary with CLI argument parsing (`--mode router|shard|standalone`, `--config`, etc.)
- [x] 1.10 Wire up a minimal axum HTTP server in `server` with `/v1/health` returning `200 OK`
- [x] 1.11 Add CI basics: `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt --check`

**Milestone:** `cargo run -- --mode standalone` starts and responds to `/v1/health`.

## Phase 2 — Text Analysis Pipeline

Build the tokenizer and stemmer chain so text can be processed for indexing and querying.

**Depends on:** Phase 1

- [x] 2.1 Implement `Token` type and `TokenStream` iterator in `analysis`
- [x] 2.2 Implement `Tokenizer` trait and `UnicodeTokenizer` (word boundary splitting) in `analysis`
- [x] 2.3 Implement `TokenFilter` trait in `analysis`
- [x] 2.4 Implement `LowercaseFilter` (Unicode-aware) in `analysis`
- [x] 2.5 Implement `StemmerFilter` wrapping `rust-stemmers` for English, Spanish, and Portuguese in `analysis`
- [x] 2.6 Implement `Analyzer` struct that chains a `Tokenizer` + `Vec<Box<dyn TokenFilter>>` in `analysis`
- [x] 2.7 Implement `AnalyzerFactory` that builds the correct `Analyzer` from `AnalyzerType` config
- [x] 2.8 Unit tests: tokenizer splits correctly on Unicode, punctuation, numbers
- [x] 2.9 Unit tests: stemmer produces expected stems for EN/ES/PT (singular/plural, verb forms)
- [x] 2.10 Unit tests: full pipeline end-to-end ("Samsung Galaxy S24" → ["samsung", "galaxi", "s24"])

**Milestone:** `Analyzer` processes text into token streams for all 3 languages.

## Phase 3 — Index Structures (On-Disk)

Build the three storage layers: inverted index, column store, and document store. Each is a segment-level component.

**Depends on:** Phase 2

### Inverted Index
- [x] 3.1 Implement posting list structure: doc_id + term_frequency, serializable
- [x] 3.2 Implement PFOR-delta compression/decompression for posting lists
- [x] 3.3 Implement FST-based term dictionary writer (term → term_id mapping)
- [x] 3.4 Implement FST-based term dictionary reader with exact lookup
- [x] 3.5 Implement Levenshtein automaton integration with FST for fuzzy lookup
- [x] 3.6 Implement inverted index writer: accepts analyzed tokens, builds term dict + posting lists, writes to segment files
- [x] 3.7 Implement inverted index reader: loads segment, performs term lookups, returns posting lists
- [x] 3.8 Unit tests: round-trip write/read of inverted index with known documents
- [x] 3.9 Unit tests: fuzzy lookup returns correct terms within edit distance

### Column Store
- [x] 3.10 Implement dictionary-encoded keyword column writer (value → ordinal mapping + ordinal array)
- [x] 3.11 Implement numeric column writer (packed f64 array)
- [x] 3.12 Implement column reader with filter evaluation: equality, range, multi-value IN
- [x] 3.13 Implement bitset-based filter result combination (AND/OR)
- [x] 3.14 Unit tests: keyword column round-trip, filter evaluation
- [x] 3.15 Unit tests: numeric range filter evaluation

### Document Store
- [x] 3.16 Implement document store writer: serialize documents into blocks with doc-id-to-offset index
- [x] 3.17 Implement document store reader: retrieve document by doc_id
- [x] 3.18 Implement deletion bitmap (roaring bitmap) per segment
- [x] 3.19 Unit tests: document store round-trip, deletion bitmap excludes deleted docs

### Segment
- [x] 3.20 Define `Segment` struct combining inverted index, column store, and doc store
- [x] 3.21 Implement `SegmentWriter`: takes documents, runs through analysis, builds all three stores, writes to disk
- [x] 3.22 Implement `SegmentReader`: opens a segment directory, exposes search/filter/retrieve operations
- [x] 3.23 Implement segment metadata (doc count, term count, min/max doc_id) in `meta.json`
- [x] 3.24 Integration test: write 1000 documents to a segment, read them back, verify search and filters

**Milestone:** A single segment can be written and searched with full-text, filters, and document retrieval.

## Phase 4 — Shard Engine (WAL, Write Buffer, Multi-Segment Search)

Build the shard engine that manages segments, WAL, real-time writes, and local search.

**Depends on:** Phase 3

### Write Path
- [ ] 4.1 Implement WAL: append-only log with write operations (index, update, delete), fsync, and truncation
- [ ] 4.2 Implement WAL recovery: replay log entries into write buffer on startup
- [ ] 4.3 Implement in-memory write buffer: accepts documents, supports search over buffered docs
- [ ] 4.4 Implement write buffer flush: convert buffer to immutable segment on disk when size threshold reached
- [ ] 4.5 Implement double-buffer swap: readers see old buffer while new one accumulates, atomic swap on flush
- [ ] 4.6 Unit tests: WAL write + crash recovery produces consistent state
- [ ] 4.7 Unit tests: write buffer search returns recently indexed documents

### Read Path
- [ ] 4.8 Implement multi-segment search: query all segments + write buffer, merge results by score
- [ ] 4.9 Implement deletion-aware search: apply deletion bitmap to exclude deleted docs across segments
- [ ] 4.10 Implement BM25 scoring with per-field boosting (title boost: 2.0, description: 1.0)
- [ ] 4.11 Implement count aggregations: iterate column store for matching docs, compute per-value histograms
- [ ] 4.12 Implement sort by score (default), price, or other numeric fields
- [ ] 4.13 Implement pagination (offset/limit) at shard level
- [ ] 4.14 Unit tests: BM25 scoring ranks title matches above description matches
- [ ] 4.15 Unit tests: aggregation counts are correct with and without filters

### Document Lifecycle
- [ ] 4.16 Implement document update: delete old version (add to deletion bitmap) + index new version
- [ ] 4.17 Implement document delete: add to deletion bitmap, append to WAL
- [ ] 4.18 Unit tests: update replaces old doc, delete removes doc from results

### Segment Merge
- [ ] 4.19 Implement tiered merge policy: select segments to merge based on size tiers
- [ ] 4.20 Implement segment merge: combine N segments into one, applying deletion bitmaps, writing new segment
- [ ] 4.21 Implement atomic segment swap: replace merged segments with new one without blocking readers
- [ ] 4.22 Implement background merge task (tokio spawn)
- [ ] 4.23 Unit tests: merge produces correct results, deleted docs are physically removed

### Shard
- [ ] 4.24 Implement `Shard` struct tying together WAL, write buffer, segment list, merge scheduler
- [ ] 4.25 Implement shard startup: load segments from disk, replay WAL, start merge background task
- [ ] 4.26 Implement shard shutdown: flush write buffer, sync WAL
- [ ] 4.27 Integration test: index 10K documents, search, update, delete, verify correctness
- [ ] 4.28 Integration test: kill process, restart, verify no data loss (WAL recovery)

**Milestone:** A single shard can index, search, update, delete with real-time visibility and crash resilience.

## Phase 5 — Pluggable Re-Ranking

Build the ranker abstraction and all three implementations before wiring them into the router.

**Depends on:** Phase 1 (core types + proto only; can be developed in parallel with Phases 2-4)

### Ranker Trait and NoopRanker
- [ ] 5.1 Define `Ranker` async trait in `ranker` crate: `rerank(query, candidates) -> Vec<RankedResult>`
- [ ] 5.2 Implement `NoopRanker`: returns candidates in original BM25 order unchanged
- [ ] 5.3 Implement `rerank_with_fallback` wrapper: applies timeout (default 30ms), falls back to BM25 order on timeout/error
- [ ] 5.4 Unit tests: NoopRanker preserves order, fallback triggers on timeout

### GrpcRanker
- [ ] 5.5 Implement `GrpcRanker`: wraps a tonic gRPC client calling `RankerService.Rerank`
- [ ] 5.6 Implement connection management: connection pooling, reconnect on failure
- [ ] 5.7 Create a mock gRPC ranker server for testing (reverses BM25 order or boosts by price)
- [ ] 5.8 Integration test: GrpcRanker calls mock server, receives re-ranked results
- [ ] 5.9 Integration test: GrpcRanker times out after 30ms, falls back to BM25

### WasmRanker
- [ ] 5.10 Implement `WasmRanker`: loads `.wasm` module via wasmtime at startup
- [ ] 5.11 Define WASM guest API contract: exported `rerank` function, memory layout for candidates/results
- [ ] 5.12 Implement host↔guest data serialization (candidates in, ranked results out)
- [ ] 5.13 Configure wasmtime resource limits: max memory, fuel-based CPU limiting
- [ ] 5.14 Create a sample WASM ranker module (Rust compiled to wasm32-wasi) for testing
- [ ] 5.15 Unit tests: WasmRanker loads module and re-ranks correctly
- [ ] 5.16 Unit tests: WasmRanker respects memory and CPU limits

### RankerFactory
- [ ] 5.17 Implement `RankerFactory`: builds the correct `Ranker` from config (`none` → Noop, `grpc` → Grpc, `wasm` → Wasm)
- [ ] 5.18 Unit tests: factory creates correct implementation for each config type

**Milestone:** All three ranker implementations work standalone with timeout/fallback.

## Phase 6 — Distributed Layer (gRPC, Router, Sharding)

Connect shards via gRPC, implement the router with scatter-gather and re-ranking, and enable multi-shard operation.

**Depends on:** Phase 4, Phase 5

### gRPC Shard Server
- [ ] 6.1 Implement `ShardGrpcServer` in `shard` crate: wraps `Shard` and exposes it via the tonic-generated `ShardService` trait
- [ ] 6.2 Implement `Search` RPC handler: translate protobuf request → shard query → protobuf response
- [ ] 6.3 Implement `Index` RPC handler: route document to local shard, return success/error
- [ ] 6.4 Implement `Delete` RPC handler
- [ ] 6.5 Implement `GetDocs` RPC handler: fetch full documents by ID list
- [ ] 6.6 Implement `Stats` and `Health` RPC handlers
- [ ] 6.7 Unit tests: gRPC server responds correctly to each RPC

### Router
- [ ] 6.8 Implement `ShardClient` trait and `RemoteShardClient` wrapping tonic gRPC client in `router`
- [ ] 6.9 Implement `LocalShardClient` for standalone mode (direct function call, same protobuf types)
- [ ] 6.10 Implement jump consistent hash for document-to-shard routing
- [ ] 6.11 Implement scatter-gather search: fan out query to all shards in parallel, collect responses
- [ ] 6.12 Implement result merge: priority queue across shard results, global top-N candidates by score
- [ ] 6.13 Implement aggregation merge: sum counts across shards per field value
- [ ] 6.14 Integrate `Ranker` into query flow: after merge, send top-N candidates to ranker, apply result
- [ ] 6.15 Add `reranked: bool` to search response
- [ ] 6.16 Implement global pagination: apply offset/limit after re-ranking
- [ ] 6.17 Implement write routing: compute shard for doc_id, forward index/delete to correct shard
- [ ] 6.18 Implement shard health checking and timeout handling (return partial results on shard failure)
- [ ] 6.19 Implement shard discovery: static config and DNS-based (K8s headless service)
- [ ] 6.20 Unit tests: jump consistent hash distributes uniformly
- [ ] 6.21 Integration test: 4-shard standalone cluster, index 10K docs, search returns correct merged results
- [ ] 6.22 Integration test: aggregation counts sum correctly across shards
- [ ] 6.23 Integration test: one shard down, router returns partial results with degradation flag
- [ ] 6.24 Integration test: re-ranking applied after merge, results reordered, `reranked: true` in response
- [ ] 6.25 Integration test: ranker timeout, BM25 fallback, `reranked: false` in response

### HTTP API
- [ ] 6.26 Implement `POST /v1/search` endpoint in `server`: parse JSON request, call router, return JSON response
- [ ] 6.27 Implement `POST /v1/index` endpoint: parse document, route to shard via router
- [ ] 6.28 Implement `DELETE /v1/index/{id}` endpoint
- [ ] 6.29 Implement `GET /v1/stats` endpoint: aggregate stats from all shards
- [ ] 6.30 Implement input validation: max query length, max filters, max doc size, return 400 on invalid
- [ ] 6.31 Implement per-query timeout (default 5s)
- [ ] 6.32 End-to-end test: HTTP client → router → shards → re-ranker → response, full round trip

### Server Modes
- [ ] 6.33 Implement `--mode standalone`: start router + N in-process shard gRPC servers on localhost ports
- [ ] 6.34 Implement `--mode shard`: start a single shard gRPC server, derive shard_id from config/hostname
- [ ] 6.35 Implement `--mode router`: start HTTP server + gRPC clients to configured shard endpoints
- [ ] 6.36 Integration test: start router and shard as separate processes, verify communication

**Milestone:** Full distributed engine with pluggable re-ranking, running as separate router + shard processes, searchable via HTTP API.

## Phase 7 — Deployment and Production Readiness

Containerize, create K8s manifests, and add operational tooling.

**Depends on:** Phase 6

### Docker
- [ ] 7.1 Create multi-stage Dockerfile (rust builder → distroless runtime)
- [ ] 7.2 Verify image builds and runs in both `--mode router` and `--mode shard`
- [ ] 7.3 Optimize image size (strip binary, minimal base)

### Kubernetes Manifests
- [ ] 7.4 Create shard `StatefulSet` manifest with PVC template (SSD storage class)
- [ ] 7.5 Create shard headless `Service` manifest for DNS discovery
- [ ] 7.6 Create router `Deployment` manifest (2+ replicas)
- [ ] 7.7 Create router `ClusterIP` Service manifest
- [ ] 7.8 Add `ConfigMap` for search-engine.yaml configuration (including ranker config)
- [ ] 7.9 Add readiness/liveness probes (gRPC health for shards, HTTP /v1/health for router)
- [ ] 7.10 Add resource requests/limits for router and shard pods
- [ ] 7.11 (Optional) Create ranker `Deployment` manifest for external gRPC ranker service

### Operational
- [ ] 7.12 Implement structured logging (tracing crate) with configurable log level
- [ ] 7.13 Expose Prometheus metrics endpoint: query latency, indexing rate, shard health, segment count, WAL size, ranker latency, ranker fallback rate
- [ ] 7.14 Implement graceful shutdown: stop accepting writes, flush WAL, drain in-flight queries
- [ ] 7.15 Implement FST pre-warming on shard startup

### Validation
- [ ] 7.16 Deploy to a local K8s cluster (kind/minikube) with 4 shards and 2 routers
- [ ] 7.17 Load test: bulk index 100K documents, run search benchmarks, verify latency targets
- [ ] 7.18 Chaos test: kill a shard pod, verify WAL recovery and no data loss after restart
- [ ] 7.19 Chaos test: verify router returns partial results when a shard is down
- [ ] 7.20 Ranker test: deploy mock gRPC ranker, verify end-to-end re-ranking in K8s
- [ ] 7.21 Ranker test: stop ranker service, verify BM25 fallback within 30ms

**Milestone:** Engine deployed and validated on Kubernetes with re-ranking and operational tooling.

## Task Dependencies

```
Phase 1 (Skeleton)
    │
    ├──────────────────────┐
    ▼                      ▼
Phase 2 (Analysis)    Phase 5 (Ranker)
    │                      │
    ▼                      │
Phase 3 (Index)            │
    │                      │
    ▼                      │
Phase 4 (Shard Engine)     │
    │                      │
    ├──────────────────────┘
    ▼
Phase 6 (Distributed Layer)
    │
    ▼
Phase 7 (Deployment)
```

Phases 2→3→4 and Phase 5 can be developed **in parallel** after Phase 1. They converge at Phase 6 when the router wires together shards + ranker.

Within a phase, tasks are ordered to minimize blocking but some have internal dependencies:

- **3.20-3.24** depend on 3.1-3.19 (segment combines all stores)
- **4.8-4.15** depend on 4.1-4.5 (search depends on write path existing)
- **4.24-4.28** depend on all prior Phase 4 tasks (shard ties everything together)
- **5.5-5.9** and **5.10-5.16** can be developed in parallel (GrpcRanker and WasmRanker are independent)
- **6.14-6.16** depend on Phase 5 (ranker integration into router)
- **6.26-6.32** depend on 6.1-6.19 (HTTP API wraps router which wraps gRPC)
- **7.4-7.11** depend on 7.1-7.3 (K8s manifests reference the Docker image)
- **7.16-7.21** depend on all prior Phase 7 tasks (validation runs the full deployment)

## Risk Mitigation Tasks

These are embedded in the phases above but called out for visibility:

| Risk | Mitigating Task(s) |
|------|-------------------|
| WAL correctness / data loss | 4.6, 4.28 (WAL recovery tests, crash simulation) |
| Fuzzy search latency | 3.5, 3.9 (Levenshtein automaton correctness), 7.17 (load test) |
| Segment merge corruption | 4.20, 4.23 (merge correctness tests) |
| gRPC overhead | 6.7, 6.36 (measure latency in multi-process mode) |
| Partial failure handling | 6.18, 6.23, 7.19 (shard down scenarios) |
| Re-ranker latency / fallback | 5.3, 5.4, 5.9, 6.25, 7.21 (timeout + fallback at every layer) |
| WASM plugin safety | 5.13, 5.16 (resource limits, memory bounds) |
| K8s deployment issues | 7.16 (local cluster validation before real deployment) |
