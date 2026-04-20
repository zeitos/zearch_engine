# Design: Query Cache

## Where it fits

```
Router::search()
    │
    ├─ 1. Build cache key
    ├─ 2. Cache lookup ──► HIT: slice + optional doc fetch → return
    │         │
    │         └─ MISS:
    ├─ 3. Scatter to shards
    ├─ 4. Merge
    ├─ 5. Fetch candidate docs
    ├─ 6. Re-rank
    ├─ 7. Availability filter
    ├─ 8. Store in cache  ◄── NEW
    ├─ 9. Paginate
    └─ 10. Optional doc fetch → return
```

The cache stores the **full post-filter candidate list** (not just the page). This lets
a single cache entry serve multiple pages of the same query without a new scatter.

## Cache key

```rust
#[derive(Hash, PartialEq, Eq, Clone)]
pub struct QueryCacheKey {
    query: String,
    filters: Vec<(String, String)>,   // sorted by field name, value serialized
    aggregations: Vec<String>,
    sort: Option<(String, bool)>,     // (field, is_asc)
    offset: usize,
    limit: usize,
    typo_tolerance: bool,
    language: Option<String>,
    destination_zone: Option<String>,
}
```

`include_docs` is excluded — the cache stores `Vec<(u64, f32)>` (no full docs), so
the same entry serves both `include_docs: false` and `include_docs: true`.

`filters` is a sorted `Vec` to guarantee key equality regardless of HashMap insertion
order.

## Cached value

```rust
#[derive(Clone)]
pub struct CachedResponse {
    pub hits: Vec<(u64, f32)>,                          // full post-filter candidates
    pub total_hits: u64,
    pub aggregations: HashMap<String, Vec<AggregationBucket>>,
    pub reranked: bool,
    pub retrieval_mode: RetrievalMode,
}
```

Wrapped in `Arc<CachedResponse>` so cloning the value out of the cache is cheap.

## Cache hit path

```rust
if let Some(cached) = self.cache.get(&key).await {
    metrics::counter!("search_cache_hits_total").increment(1);
    let page_hits: Vec<(u64, f32)> = cached.hits
        .iter()
        .skip(request.offset)
        .take(request.limit)
        .copied()
        .collect();
    // optional doc fetch, build SearchResponse, return
}
```

## Implementation: moka

[moka](https://docs.rs/moka) is an async-aware, thread-safe LRU cache with TTL support.
It uses a W-TinyLFU eviction policy (better hit rate than plain LRU for Zipfian
query distributions).

```rust
use moka::future::Cache;

Cache::<QueryCacheKey, Arc<CachedResponse>>::builder()
    .max_capacity(config.max_entries)
    .time_to_live(Duration::from_secs(config.ttl_seconds))
    .build()
```

## Configuration

```rust
pub struct QueryCacheConfig {
    pub enabled: bool,           // default: false
    pub max_entries: u64,        // default: 10_000
    pub ttl_seconds: u64,        // default: 60
}
```

```yaml
router:
  cache:
    enabled: true
    max_entries: 10000
    ttl_seconds: 60
```

## SearchResponse extension

```rust
pub struct SearchResponse {
    // ... existing fields ...
    pub cache_hit: bool,   // new
}
```

## Metrics

| Metric | Type | Description |
|---|---|---|
| `search_cache_hits_total` | counter | Requests served from cache |
| `search_cache_misses_total` | counter | Requests that ran the full pipeline |
| `search_cache_size` | gauge | Current number of entries in cache |

## Trade-offs

| Choice | Alternative | Reason |
|---|---|---|
| In-process cache | Redis | No network hop, zero infrastructure dependency |
| Cache full candidate list | Cache only the page | Single entry serves all pages of the same query |
| TTL-only invalidation | Write invalidation | Writes don't know which cache keys they affect |
| `enabled: false` default | `enabled: true` | Cache adds memory; opt-in lets operators measure the benefit first |
| W-TinyLFU (moka) | LRU | Better hit rate for Zipfian query distributions (20% of queries = 80% of traffic) |
