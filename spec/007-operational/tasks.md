# Tasks: Operational — Graceful Shutdown + Circuit Breaker

## Status: Implemented

- [x] T-01 — `CircuitBreakerConfig { enabled, failure_threshold, recovery_ms }` in `core/config.rs`; `circuit_breaker` field on `RouterConfig`; exported from `search_core`
- [x] T-02 — `crates/core/src/breaker.rs`: `CircuitBreaker` (AtomicU32 failures, AtomicU64 opened_at_ms); Closed/Open/Half-open state; `record_success` / `record_failure`; metric name injected via constructor so each wrapper reports under its own counter (`shard_circuit_open_total`, `ranker_circuit_open_total`). Promoted to `search-core` so both router and ranker share the state machine.
- [x] T-03 — `BreakerShardClient` wraps `Arc<dyn ShardClient>`; records outcomes on `search()` and `get_docs()`; overrides `is_circuit_open()`
- [x] T-04 — `ShardClient` trait: `fn is_circuit_open(&self) -> bool { false }` default method
- [x] T-05 — `ShardGroup::read_target()` iterates endpoints from round-robin offset, skips `is_circuit_open()` ones; falls back to primary if all open
- [x] T-06 — `main.rs` router mode: wrap each `RemoteShardClient` in `BreakerShardClient` when `circuit_breaker.enabled = true`
- [x] T-07 — `main.rs` graceful shutdown: after `start_grpc_server` returns → `shard.flush()`; after `start_http_server` returns → `router.flush().await` (both Router and Standalone modes)
- [x] T-08 — Unit tests: closed by default; opens after threshold; closes on success; half-open after recovery window
- [x] T-09 — `crates/ranker/src/breaker.rs`: `BreakerRanker` wraps `Arc<dyn Ranker>`; open circuit → returns `Err` fast so `rerank_with_fallback` degrades to BM25 ordering instead of waiting for the timeout; `ranker_circuit_short_circuit_total` metric. Unit tests: opens after failures + short-circuits; passes through when closed.
- [x] T-10 — `RankerConfig.circuit_breaker: CircuitBreakerConfig` in `core/config.rs`; `RankerFactory::build` wraps remote (`Grpc`) ranker in `BreakerRanker` when `circuit_breaker.enabled = true`. Wasm/None rankers are in-process and skip the wrapper.
