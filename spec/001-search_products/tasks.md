# Tasks: Search Products

## Status: Deployed

- [x] Cargo workspace with all crates
- [x] Core types: `Document`, `Value`, `IndexSchema`, `FieldConfig`, `SearchRequest`, `SearchResponse`, `SearchHit`
- [x] Text analysis: Unicode tokenizer, lowercase filter, stemmer (EN/ES/PT), `AnalyzerFactory`
- [x] Inverted index: FST term dictionary, posting lists, Levenshtein typo tolerance
- [x] Column store: keyword and numeric columns, equality/range/multi-value filters
- [x] Document store: serialized doc blocks with offset index, roaring bitmap deletions
- [x] `SegmentWriter` / `SegmentReader`
- [x] WAL: append-only log, fsync, crash recovery on open
- [x] Write buffer: in-memory buffer with searchable temp segment, auto-flush on capacity
- [x] `ShardEngine`: WAL + write buffer + segment list + background merge
- [x] BM25 scoring with per-field boost (title 2.0, description 1.0)
- [x] Multi-segment search: fan-out, deletion-aware, merge by score
- [x] Aggregations: facet counts per field across segments
- [x] Sort: by score, price, or any numeric field
- [x] Pagination at shard level
- [x] `ShardClient` trait: `LocalShardClient` (in-process), `RemoteShardClient` (gRPC)
- [x] Router: jump consistent hash, scatter-gather fan-out, aggregation merge
- [x] Re-ranker wiring: top-N candidates → ranker → apply result; fallback to BM25 on timeout
- [x] `NoopRanker`, `GrpcRanker`, `WasmRanker` (wasmtime)
- [x] gRPC shard server: `Search`, `GetDocs`, `Index`, `BulkIndex`, `Delete`, `Stats`, `Health`, `Flush`
- [x] HTTP API: `POST /v1/search`, `POST /v1/index`, `POST /v1/bulk`, `DELETE /v1/index/{id}`, `GET /v1/stats`, `GET /v1/health`, `POST /v1/admin/flush`, `POST /v1/ingest/bulk`
- [x] Standalone, shard, and router modes
- [x] Prometheus metrics
- [x] Admin UI
- [x] `include_docs: bool` — optional full document hydration in search response (default false)
- [x] Per-shard query timeout wired into scatter fan-out
