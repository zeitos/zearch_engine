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

## Perf hardening (post-implementation)

- [x] T-16 — `SuggestIndex::query`: quickselect (`select_nth_unstable_by`) for top-K over matching prefix range; clone only the final `limit` strings. Cuts complexity from O(M log M) to O(M) avg on broad prefixes over large term sets.
- [x] T-17 — `SuggestIndexBuilder::add_document`: sort + dedup the analyzed tokens of a document instead of allocating a per-doc `HashSet`. Avoids one hash table + one string clone per term per doc.
- [x] T-18 — `extract_field` returns `Cow<'a, str>`: title/description/category are borrowed from the `Document`; only `StringArray` attributes allocate (for `join`). Removes the per-document per-field `String::clone`.
- [x] T-19 — Builder stores terms as `Arc<str>` (`HashMap<Arc<str>, u32>`) and `SuggestIndex` holds `Vec<(Arc<str>, f32)>`. Flush clones refcounts, not string bytes — 10k-term snapshots go from O(M) allocations to zero. Top-K query still converts to `String` for the (small) response payload.
- [x] T-20 — `Router::fan_out_suggest_write`: hash-partition docs by `doc_id` into one `Vec<Document>` per suggest shard, then `tokio::spawn` one fire-and-forget bulk per shard. Cuts doc clones from N-way (one per shard) to 1-way (one per doc) regardless of suggest shard count.
- [x] T-21 — HTTP `GET /v1/suggest` handler: typed `#[derive(Serialize)] struct SuggestResponse` instead of the `serde_json::json!` macro. Avoids building a `serde_json::Value` tree before serialization.
