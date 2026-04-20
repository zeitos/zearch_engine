# Design: Pluggeable Availability Strategy

## Architecture

```
HTTP Request { query, destination_zone, ... }
        │
        ▼
   Router::search()
        │
        ├─ 1. Scatter to shards  (limit = ranker_candidates × multiplier)
        ├─ 2. Merge scored hits
        ├─ 3. Fetch candidate docs
        ├─ 4. Re-rank (optional)
        ├─ 5. Apply availability filter  ◄── NEW
        ├─ 6. Paginate (offset / limit)
        └─ 7. Return hits
```

Availability filtering slots in **after re-ranking and before pagination**. By that
point all candidate doc metadata is already in memory, so `StaticAvailabilityStrategy`
is essentially free. `RegionStockStrategy` makes one batched HTTP call per query.

## Trait

```rust
// crates/router/src/availability.rs

#[async_trait]
pub trait AvailabilityStrategy: Send + Sync {
    /// Remove unavailable candidates from `candidates` in-place.
    /// `docs` contains full document metadata for each candidate id.
    /// Called only when `zone` is non-empty.
    async fn apply(
        &self,
        candidates: &mut Vec<(u64, f32)>,
        docs: &HashMap<u64, &Document>,
        zone: &str,
    );

    /// Inflation factor for the scatter fetch.
    /// A strategy that filters ~66% of docs should return 3
    /// so the post-filter candidate pool stays ≥ ranker_candidates.
    fn candidate_multiplier(&self) -> usize {
        1
    }
}
```

## Implementations

### NoopStrategy

```rust
pub struct NoopStrategy;

impl AvailabilityStrategy for NoopStrategy {
    async fn apply(&self, _: &mut Vec<(u64, f32)>, _: &HashMap<u64, &Document>, _: &str) {}
}
```

### StaticAvailabilityStrategy

Reads stock from document attributes. No network calls.

Config fields:
- `stock_attribute_prefix: String` — e.g. `"stock_"` → looks up `"stock_{zone}"`
- `default_available: bool` — what to return when attribute is absent (default `true`)
- `min_stock: f64` — minimum numeric value to consider in-stock (default `1.0`)

Document side (example):
```json
{
  "id": 42,
  "attributes": {
    "stock_buenos_aires": 15,
    "stock_santa_cruz": 0,
    "stock_cordoba": 8
  }
}
```

### RegionStockStrategy

Batches all candidate IDs into a single GET request:

```
GET {base_url}/availability?zone={zone}&doc_ids=1,2,3,4,5
← 200 { "available_ids": [1, 3, 5] }
```

- `timeout_ms`: hard cap on the HTTP call (default 50ms). On timeout or any error → fail-open (all candidates pass).
- Uses a persistent `reqwest::Client` (connection pooled).

## Configuration

`core/src/config.rs`:

```rust
pub struct AvailabilityConfig {
    pub strategy: AvailabilityStrategyType,  // noop | static | region_stock
    pub r#static: Option<StaticAvailabilityConfig>,
    pub region_stock: Option<RegionStockConfig>,
}

pub struct StaticAvailabilityConfig {
    pub stock_attribute_prefix: String,  // default: "stock_"
    pub default_available: bool,          // default: true
    pub min_stock: f64,                   // default: 1.0
}

pub struct RegionStockConfig {
    pub base_url: String,
    pub timeout_ms: u64,                  // default: 50
}
```

Example YAML (router ConfigMap):

```yaml
router:
  availability:
    strategy: static
    static:
      stock_attribute_prefix: "stock_"
      default_available: true
      min_stock: 1
```

```yaml
router:
  availability:
    strategy: region_stock
    region_stock:
      base_url: "http://stock-service.search.svc.cluster.local:8080"
      timeout_ms: 50
```

## Router Integration

`Router` gains a `strategy: Arc<dyn AvailabilityStrategy>` field.

The scatter limit uses the multiplier:

```rust
shard_req.limit = self.ranker_candidates * self.strategy.candidate_multiplier();
```

After re-ranking:

```rust
if let Some(zone) = &request.destination_zone {
    if !zone.is_empty() {
        self.strategy.apply(&mut final_hits, &candidate_doc_map, zone).await;
    }
}
```

The candidate doc map (`candidate_doc_map`) is already built for the re-ranker — no
additional fetch needed when using `StaticAvailabilityStrategy`. When re-ranking is
disabled (NoopRanker), the docs are fetched immediately before the availability call.

## SearchRequest Extension

```rust
pub struct SearchRequest {
    // ... existing fields ...
    pub destination_zone: Option<String>,
}
```

The field is optional with `#[serde(default)]`. Existing clients that don't send it
get `None`, which bypasses availability filtering entirely.

## Metrics

| Name | Type | Description |
|------|------|-------------|
| `availability_filter_removed_total` | counter | Candidates removed by the strategy |
| `availability_filter_duration_seconds` | histogram | Time spent in `apply()` |
| `availability_stock_service_errors_total` | counter | RegionStockStrategy timeouts / errors |

## Trade-offs

| Choice | Alternative | Reason |
|--------|-------------|--------|
| Filter after re-rank | Filter before re-rank | Re-ranker already fetched docs; filtering earlier needs an extra fetch |
| Fail-open on stock error | Fail-closed | Stock service downtime should not break search |
| `candidate_multiplier` at scatter | Over-fetch at availability call | Fetching more from shards is cheap (parallel); over-fetching after is serial |
| Trait object (`dyn`) | Generic param | Config-driven strategy selection; single router instance serves all queries |
