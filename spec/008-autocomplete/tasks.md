# Tasks: Autocomplete / Suggest

## Status: Implemented

- [x] T-01 — `SuggestShardConfig { grpc_port, shard_id, num_suggest_shards, min_doc_frequency, max_terms_per_shard, fields }` in `core/config.rs`; `Mode::Suggest` variant; `suggest_shards`, `suggest_write_timeout_ms`, `suggest_query_timeout_ms` fields on `RouterConfig`
- [x] T-02 — New crate `search-suggest`: `SuggestTerm`, `SuggestIndex` (sorted `Vec<(String, f32)>` + binary-search `query()`), `SuggestIndexBuilder` (accumulates term doc_freq from documents, normalizes, filters, builds)
- [x] T-03 — Protobuf: add `Suggest` RPC + `SuggestRequest` / `SuggestResponse` / `SuggestEntry` to `shard.proto`; regenerated
- [x] T-04 — `ShardClient` trait: `async fn suggest(prefix, field, limit) → Result<Vec<SuggestTerm>>` with default `Ok(vec![])`
- [x] T-05 — `SuggestShardEngine` in `search-suggest`: holds a `SuggestIndexBuilder` (write buffer) and a current `Arc<SuggestIndex>`; `index(doc)` adds to builder; `flush()` snapshots builder into the index; `suggest(prefix, field, limit)` queries current index
- [x] T-06 — `SuggestGrpcServer` in `search-suggest`: implements `ShardService`; handles `Index`, `BulkIndex`, `Suggest`, `Flush`, `Stats`, `Health`; returns `Unimplemented` for Search/GetDocs/Delete/Reindex/Replicate
- [x] T-07 — `LocalSuggestClient` wrapping `Arc<SuggestShardEngine>`; implements `ShardClient` (suggest + write methods); used in standalone mode
- [x] T-08 — `RemoteShardClient::suggest()` using the gRPC RPC
- [x] T-09 — `Router`: `suggest_shards: Vec<Arc<dyn ShardClient>>`; `Router::suggest()` fans out, merges by max-score per term, sorts, truncates; `Router::index()` and `bulk_index()` forward fire-and-forget with `suggest_write_timeout`
- [x] T-10 — `main.rs`: `Mode::Suggest` branch starts `SuggestShardEngine` + gRPC server with background 5s flush loop; router/standalone modes wire `suggest_shards` from config
- [x] T-11 — HTTP: `GET /v1/suggest` handler; validates `q` (≤100), `limit` (1–20, default 5), `field` (default `"title"`); returns `{ suggestions, consistency: "eventual", took_ms }`
- [x] T-12 — Metrics: `suggest_requests_total`, `suggest_duration_seconds`, `suggest_write_errors_total`, `suggest_index_terms` gauge, `suggest_index_build_seconds`
- [x] T-13 — K8s: `k8s/suggest-statefulset.yaml` (2 replicas, 256Mi RAM), `k8s/suggest-service.yaml` (headless, port 9002); `suggest-config.yaml` key added to ConfigMap; suggest endpoints added to router config
- [x] T-14 — Unit tests: `SuggestIndex::query` prefix match, case-insensitive, score ordering, empty prefix, `min_doc_frequency` filter; `SuggestIndexBuilder` from real documents; `SuggestShardEngine` index+flush+query
- [x] T-15 — Integration tests (router crate): fan-out index via router, assert "wireless" ranked above "wired"; assert suggest write failure does not fail index call
