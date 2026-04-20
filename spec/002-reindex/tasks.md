# Tasks: In-Place Reindex

## Status: Deployed

- [x] T-01 — `SegmentList::replace_all`: swap in new segment, drop all old ones, delete old dirs from disk
- [x] T-02 — `Wal::truncate`: overwrite WAL with empty file after reindex
- [x] T-03 — `ShardEngine::reindex_self`: flush → collect all docs → write new segment → replace_all → truncate WAL; returns `ReindexStats`
- [x] T-04 — `ShardClient::reindex` trait method + `LocalShardClient` + `RemoteShardClient` impls
- [x] T-05 — `Router::reindex`: fan-out to all shards in parallel, collect per-shard stats
- [x] T-06 — gRPC: `Reindex` RPC in proto + handler in `grpc_server.rs`
- [x] T-07 — HTTP: `POST /v1/admin/reindex`
- [x] T-08 — Tests: single-shard reindex preserves doc count; old segment dirs deleted; router-level reindex across shards
