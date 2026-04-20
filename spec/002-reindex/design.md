# Technical Design: In-Place Reindex

## 1. Overview

Reindex rebuilds a shard's inverted index and column store from its document store without external data ingestion. The operation is implemented entirely inside `ShardEngine` and exposed via the existing admin HTTP API.

```
POST /v1/admin/reindex
        │
        ▼
    Router
        │  (fan-out via ShardClient::reindex)
        ├──► Shard 0: reindex_self()
        ├──► Shard 1: reindex_self()
        └──► Shard N: reindex_self()
```

## 2. Shard-Level Algorithm

```
fn reindex_self() -> Result<ReindexStats>
```

### Step 1 — Flush write buffer
Force any in-memory documents into a segment so they are visible to the doc store iterator.

```
self.flush()?;
```

### Step 2 — Collect all live documents
Iterate every segment in the segment list. For each segment, walk `all_live_docs()` and call `get_doc(local_id)` to retrieve the stored document.

```
let mut docs: Vec<Document> = vec![];
for (segment_id, reader) in segment_list.readers() {
    for local_id in reader.all_live_docs() {
        if let Some(doc) = reader.get_doc(local_id) {
            docs.push(doc);
        }
    }
}
```

### Step 3 — Write new segment
Pass all collected documents to `SegmentWriter`, which re-analyzes them with the current schema, computes `avgdl`, builds inverted index with Block-Max WAND blocks, and writes the new segment to a temp directory.

```
let new_segment_id = Uuid::new_v4().to_string();
let mut writer = SegmentWriter::new(self.schema.clone());
writer.write_segment(&docs, &self.config.data_dir, &new_segment_id)?;
```

### Step 4 — Atomic swap
Hold the write lock on the segment list, add the new segment, remove all old segment IDs in a single operation. The segment list's `replace_all` method handles this atomically under the lock.

```
segment_list.replace_all(old_segment_ids, new_segment_id)?;
```

Search queries that arrive between step 3 and step 4 continue reading from old segments. After the swap, all new queries read from the new segment.

### Step 5 — Truncate WAL
Since all documents are now in the new segment, the WAL is redundant. Truncate it to reclaim disk space.

```
self.wal.truncate()?;
```

## 3. API

### Request
```
POST /v1/admin/reindex
Content-Type: application/json

{}   (no body required)
```

### Response
```json
{
  "ok": true,
  "shards": [
    { "shard_id": 0, "docs_reindexed": 27500, "elapsed_ms": 1200 },
    { "shard_id": 1, "docs_reindexed": 27500, "elapsed_ms": 1180 },
    { "shard_id": 2, "docs_reindexed": 25000, "elapsed_ms": 1100 },
    { "shard_id": 3, "docs_reindexed": 20000, "elapsed_ms": 950 }
  ],
  "total_docs": 100000,
  "total_elapsed_ms": 1220
}
```

### Error
```json
{ "error": "shard 2: failed to write new segment: disk full" }
```

## 4. ShardClient Trait Extension

Add one method to the trait:

```rust
async fn reindex(&self) -> Result<ReindexStats>;
```

`ReindexStats`:
```rust
pub struct ReindexStats {
    pub shard_id: u32,
    pub docs_reindexed: u64,
    pub elapsed_ms: u64,
}
```

`LocalShardClient` delegates to `ShardEngine::reindex_self()`.  
`RemoteShardClient` sends a new gRPC message `ReindexRequest` / `ReindexResponse`.

## 5. Concurrency

- Incoming index/delete writes during reindex are accepted normally (they go to the write buffer / WAL as usual).
- Writes that arrive after step 1 (flush) but before step 4 (swap) are in the new write buffer and will be in the next flush — no data loss.
- Search queries never block; they read from whatever segments are currently registered.

## 6. Files Changed

| File | Change |
|------|--------|
| `crates/shard/src/shard.rs` | Add `reindex_self() -> Result<ReindexStats>` |
| `crates/shard/src/segment_list.rs` | Add `replace_all(old_ids, new_id)` |
| `crates/shard/src/wal.rs` | Add `truncate()` |
| `crates/router/src/client.rs` | Add `reindex()` to `ShardClient` trait; implement for Local and Remote |
| `crates/router/src/router.rs` | Add `Router::reindex()` — fan-out to all shards in parallel |
| `crates/server/src/main.rs` | Add `POST /v1/admin/reindex` handler |
| `crates/proto/proto/shard.proto` | Add `Reindex` RPC |
