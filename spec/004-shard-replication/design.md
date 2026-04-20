# Technical Design: Shard Replication

## 1. Architecture

```
Client
  │
  ▼
Router
  │  writes → primary only
  │  reads  → any healthy endpoint (round-robin)
  │
  ├──► Shard 0 Primary  ──async queue──► Shard 0 Replica A
  │                     ──async queue──► Shard 0 Replica B
  │
  ├──► Shard 1 Primary  ──async queue──► Shard 1 Replica A
  └──► ...
```

### Write path
1. Router sends write to primary gRPC endpoint (existing `Index` / `BulkIndex` / `Delete` RPCs).
2. Primary writes to WAL + write buffer, returns ACK.
3. Primary `ReplicationManager` picks up the entry from a background channel and forwards it to each replica via gRPC `Replicate` RPC.
4. Replica applies the write directly to its `ShardEngine` (no WAL on replica — primary WAL is the source of truth).

### Read path
Router holds a `ShardGroup` per logical shard: one primary + list of healthy replicas. On each search, it picks one endpoint at random (or round-robin).

## 2. New Types

### `ReplicationManager` (inside shard crate)
Runs in the primary. One per replica endpoint.

```rust
pub struct ReplicationManager {
    replica_endpoints: Vec<String>,
    // per-replica unbounded channel sender
    senders: Vec<tokio::sync::mpsc::UnboundedSender<ReplicaEntry>>,
}

enum ReplicaEntry {
    Index(Document),
    Delete(u64),
    Flush,
}
```

Each replica has a dedicated Tokio task that drains its channel and calls `ReplicateRpc` on the replica's gRPC endpoint. If the call fails, it retries with exponential backoff. The task is spawned when `ShardEngine` opens in primary mode.

### `ShardGroup` (router crate)
Replaces the current single `Arc<dyn ShardClient>` per shard with a group.

```rust
pub struct ShardGroup {
    primary: Arc<dyn ShardClient>,
    replicas: Vec<Arc<dyn ShardClient>>,
    next: AtomicUsize,  // round-robin counter
}

impl ShardGroup {
    /// Always returns primary for writes.
    pub fn write_target(&self) -> &Arc<dyn ShardClient> { &self.primary }

    /// Round-robin across primary + healthy replicas for reads.
    pub fn read_target(&self) -> &Arc<dyn ShardClient> {
        let all: Vec<_> = std::iter::once(&self.primary)
            .chain(self.replicas.iter())
            .collect();
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % all.len();
        all[idx]
    }
}
```

## 3. WAL-Based Catch-Up

When a replica restarts, it reports its last known WAL sequence number (stored in a local file `replica_state.bin`) to the primary via a new `ReplicateCatchUp` RPC. The primary streams all WAL entries after that sequence number.

```proto
rpc ReplicateCatchUp (CatchUpRequest) returns (stream ReplicateRequest);

message CatchUpRequest {
    uint64 from_wal_seq = 1;
}
```

The replica marks itself as `syncing = true` and does not appear in the router's read pool until it receives a `CatchUpComplete` signal (a special entry at the end of the stream).

## 4. Proto Changes

```proto
// New RPC on ShardService
rpc Replicate (ReplicateRequest) returns (ReplicateResponse);
rpc ReplicateCatchUp (CatchUpRequest) returns (stream ReplicateRequest);

message ReplicateRequest {
    oneof operation {
        DocumentProto index = 1;
        uint64 delete_doc_id = 2;
        bool flush = 3;
    }
    uint64 wal_seq = 4;  // sequence number for catch-up tracking
}

message ReplicateResponse {
    bool ok = 1;
}

message CatchUpRequest {
    uint64 from_wal_seq = 1;
}
```

## 5. Config Changes

```yaml
# shard-config.yaml
shard_id: 0
role: primary                          # primary | replica
replica_endpoints:                      # only on primary
  - "http://shard-0-replica-a:9001"
  - "http://shard-0-replica-b:9001"
primary_endpoint: "http://shard-0:9001" # only on replica
```

Router config:
```yaml
shards:
  - primary: "http://shard-0:9001"
    replicas:
      - "http://shard-0-replica-a:9001"
      - "http://shard-0-replica-b:9001"
  - primary: "http://shard-1:9001"
    replicas: []
```

## 6. K8s Changes

Each shard gets a second StatefulSet (`shard-replica`) with its own PVCs, pointing to the primary endpoint. The router ConfigMap is updated with replica addresses.

Alternatively, replicas are just additional pods in the same StatefulSet with `role: replica` in their config — determined by ordinal (e.g. shard-0 = primary, shard-4 = replica of shard-0 for a 4-shard cluster with replication factor 2).

## 7. Files Changed

| File | Change |
|------|--------|
| `proto/shard.proto` | Add `Replicate`, `ReplicateCatchUp` RPCs and messages |
| `crates/shard/src/replication.rs` | New: `ReplicationManager`, per-replica background task |
| `crates/shard/src/shard.rs` | Primary: after WAL write, enqueue to `ReplicationManager` |
| `crates/shard/src/grpc_server.rs` | Add `Replicate` and `ReplicateCatchUp` handlers |
| `crates/shard/src/wal.rs` | Add WAL sequence numbers; add `read_since(seq)` for catch-up |
| `crates/router/src/client.rs` | Add `ShardGroup`; router uses `write_target` / `read_target` |
| `crates/router/src/router.rs` | Replace `Vec<Arc<dyn ShardClient>>` with `Vec<ShardGroup>` |
| `crates/core/src/config.rs` | Add `role`, `replica_endpoints`, `primary_endpoint` to `ShardConfig` |
| `k8s/shard-statefulset.yaml` | Add replica StatefulSet or increase replicas with role config |
| `k8s/configmap.yaml` | Add replica endpoints to router config |
