# Tasks: Shard Replication

## Status: Deployed

- [x] T-01 — WAL sequence numbers: `u64` seq per entry, `wal_records_since(seq)` for catch-up streaming
- [x] T-02 — Config: `ShardRole` (Primary/Replica), `replica_endpoints`, `primary_endpoint`, `replica_endpoint_template`, `primary_endpoint_template`; router config gains `shards: Vec<ShardGroupConfig>`
- [x] T-03 — Proto: `Replicate` RPC, `ReplicateCatchUp` server-streaming RPC, `FullSync` server-streaming RPC
- [x] T-04 — `ReplicationManager`: one mpsc channel + one background task per replica; exponential backoff on failure
- [x] T-05 — Primary enqueues into `ReplicationManager` after each WAL append (`index`, `index_batch`, `delete`)
- [x] T-06 — Replica gRPC handler: `Replicate` applies Index/Delete/Flush directly, bypassing WAL; rejects if called on primary
- [x] T-07 — Catch-up on replica startup: `ReplicateCatchUp` streams missed WAL entries; `FullSync` streams all live segment docs when WAL is empty and replica has no data; replica flushes after full sync
- [x] T-08 — `ShardGroup { primary, replicas, next: AtomicUsize }` in router; `write_target()` always primary; `read_target()` round-robin
- [x] T-09 — Router uses `Vec<ShardGroup>`; writes → `write_target()`; reads → `read_target()`; `stats()` reports primary + per-replica stats separately
- [x] T-10 — K8s: `shard-replica` StatefulSet (4 pods), headless service, ConfigMap with template-based endpoint resolution from HOSTNAME ordinal
- [x] T-11 — Tests: `ReplicationManager` noop on empty; replica count correct; `apply_replicated` does not write WAL; `is_replica` flag; gRPC `Replicate` rejected on primary; replica catches up on startup; delete replicates to replica; router read round-robin
