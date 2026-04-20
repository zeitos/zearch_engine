# Design: Autocomplete / Suggest

## Architecture

```
                    ┌─────────────────┐
  HTTP clients ────►│     Router      │
                    └────────┬────────┘
                             │
              ┌──────────────┴──────────────┐
              │ search fan-out              │ suggest fan-out (fire-and-forget)
              ▼                             ▼
   ┌─────────────────┐           ┌─────────────────┐
   │  Search Shard 0 │  ...      │ Suggest Shard 0 │  ...
   │  (WAL, durable) │           │ (no WAL, lossy) │
   └─────────────────┘           └─────────────────┘
```

### Write path

The router fans out `index` and `bulk` to **both** clusters in parallel.
Suggest writes use a short timeout (`write_timeout_ms`, default 200ms) and are
dropped silently on failure — the caller never sees suggest write errors.

```rust
// Pseudo-code in Router::index()
let search_write = search_shard.index(doc.clone());
let suggest_write = tokio::time::timeout(
    self.suggest_write_timeout,
    suggest_shard.index(doc),
);
search_write.await?;                      // propagates errors
let _ = suggest_write.await;              // swallowed
```

### Read path

`GET /v1/suggest` fans out to all suggest shards, merges, and returns. Completely
independent of the search cluster.

```
GET /v1/suggest?q=wire&limit=5
        │
        ▼
   Router::suggest()
        │  fan-out to suggest shards only
        ├──────────────────────────┐
        ▼                          ▼
  SuggestShard 0             SuggestShard N
  → [("wireless",0.9), ...]  → [("wireless",0.8), ...]
        │                          │
        └──────────┬───────────────┘
                   ▼
        merge by max-score per term
        sort desc, truncate to limit
        → [("wireless",0.9), ("wired",0.6), ...]
```

## Suggest Shard Mode

The same binary gains `mode: suggest`. In this mode:

- Starts a gRPC server (same port as search shards, configurable)
- Accepts `Index`, `BulkIndex` — writes go to an in-memory `SuggestIndex`, no WAL
- Exposes a `Suggest` gRPC method
- Does NOT implement `Search`, `GetDocs`, `Reindex`, `Stats` (or returns `Unimplemented`)
- Flush rebuilds the `SuggestIndex` from current state (optional, for memory cleanup)

## `SuggestIndex`

New crate `search-suggest`:

```rust
pub struct SuggestIndex {
    // Sorted lexicographically by term. Built at flush time.
    terms: Vec<(String, f32)>,
}

impl SuggestIndex {
    pub fn query(&self, prefix: &str, limit: usize) -> Vec<SuggestTerm> {
        let prefix = prefix.to_lowercase();
        let start = self.terms.partition_point(|(t, _)| t.as_str() < prefix.as_str());
        self.terms[start..]
            .iter()
            .take_while(|(t, _)| t.starts_with(&prefix))
            .take(limit)
            .map(|(t, s)| SuggestTerm { term: t.clone(), score: *s })
            .collect()
    }
}
```

Binary search to prefix boundary → O(log N + K). For 1M terms this is ~20 comparisons
plus the result scan — well within 20ms.

## `SuggestIndexBuilder`

Accumulates term→doc_freq from incoming documents (write buffer), normalizes on build:

```rust
pub struct SuggestIndexBuilder {
    counts: HashMap<String, u32>,  // term → doc_count
}

impl SuggestIndexBuilder {
    pub fn add_document(&mut self, doc: &Document, fields: &[&str]);
    pub fn build(self, min_doc_frequency: u32) -> SuggestIndex;
}
```

`add_document` tokenizes the requested fields (title, etc.) using the standard
analyzer (lowercase + tokenize, no stemming — suggest works on surface forms).
`build` normalizes scores to [0,1], filters below `min_doc_frequency`, sorts.

## Protobuf

```protobuf
// New RPC on ShardService
rpc Suggest(SuggestRequest) returns (SuggestResponse);

message SuggestRequest {
  string prefix = 1;
  string field  = 2;
  uint32 limit  = 3;
}

message SuggestResponse {
  repeated SuggestEntry entries = 1;
}

message SuggestEntry {
  string term  = 1;
  float  score = 2;
}
```

## Router Changes

`RouterConfig` gains:

```rust
pub suggest_shards: Vec<String>,          // gRPC endpoints of suggest shards
pub suggest_write_timeout_ms: u64,        // default: 200
pub suggest_query_timeout_ms: u64,        // default: 100
```

`Router` holds a `Vec<Arc<dyn ShardClient>>` for suggest shards (no `ShardGroup` —
no replicas in v1). The `ShardClient` trait already covers what suggest shards need
(`index`, `bulk`, plus the new `suggest` method).

`ShardClient` trait gains:

```rust
async fn suggest(
    &self,
    prefix: &str,
    field: &str,
    limit: usize,
) -> search_core::Result<Vec<SuggestTerm>>;
```

Default impl returns `Ok(vec![])` so existing `LocalShardClient` / `RemoteShardClient`
for search shards compile without change.

## HTTP Endpoint

```
GET /v1/suggest?q=wire&limit=5&field=title
```

Response:

```json
{
  "suggestions": [
    { "term": "wireless", "score": 0.94 },
    { "term": "wired",    "score": 0.61 }
  ],
  "consistency": "eventual",
  "took_ms": 3
}
```

Validation: `q` required, 1–100 chars; `limit` 1–20 (default 5); `field` default
`"title"`. Returns `[]` with 200 if no shards are configured — not an error.

## Configuration

```yaml
# In router-config.yaml
router:
  suggest_shards:
    - "http://suggest-0.suggest-headless.search.svc.cluster.local:9002"
    - "http://suggest-1.suggest-headless.search.svc.cluster.local:9002"
  suggest_write_timeout_ms: 200
  suggest_query_timeout_ms: 100
```

```yaml
# suggest-config.yaml (new ConfigMap key)
mode: suggest
suggest_shard:
  grpc_port: 9002
  num_suggest_shards: 2
  min_doc_frequency: 2
  max_terms_per_shard: 50
```

## Kubernetes

Two new manifests:
- `k8s/suggest-statefulset.yaml` — 2 replicas, small resource footprint (256Mi RAM)
- `k8s/suggest-service.yaml` — headless service on port 9002

Suggest shards are not in the `shard_groups` — the router treats them as a
separate pool, contacted only for writes (fire-and-forget) and suggest queries.

## Future: Kafka Integration

When Kafka is introduced, the router fan-out is replaced by two independent consumers:

```
Kafka: catalogue.items
    ├──► Consumer A (search-indexer)  → search shards
    └──► Consumer B (suggest-indexer) → suggest shards
```

No changes to the shards themselves — they just receive `Index`/`BulkIndex` gRPC calls
from whoever is feeding them. The router stops forwarding writes entirely.

## Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `suggest_requests_total` | counter | Total suggest requests |
| `suggest_duration_seconds` | histogram | End-to-end suggest latency |
| `suggest_write_errors_total` | counter | Dropped suggest writes (fire-and-forget failures) |
| `suggest_index_terms` | gauge | Terms in suggest index (label: `shard_id`) |
| `suggest_index_build_seconds` | histogram | Rebuild time after flush |

## Trade-offs

| Choice | Alternative | Reason |
|--------|-------------|--------|
| No WAL on suggest shards | WAL like search | Suggest is lossy by design; WAL adds complexity without fitting the SLA contract |
| Fire-and-forget writes from router | Synchronous writes to both | Suggest write latency must not block the caller |
| Independent suggest cluster | Co-locate with search shards | Independent SLA, scaling, deployment; failure isolation |
| No replicas in v1 | Replicated suggest shards | Suggest data is cheap to rebuild; replication adds operational complexity for marginal gain |
| Two consumers (future) vs router fan-out (now) | Router fan-out always | Kafka consumers decouple ingestion from query path; router fan-out is the simplest starting point |
