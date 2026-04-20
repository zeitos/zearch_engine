# Tasks: Query Cache

## Status: Implemented — pending deploy

- [x] T-01 — `moka` (W-TinyLFU, async, TTL) added to workspace and `search-router`
- [x] T-02 — `QueryCacheConfig { enabled, max_entries, ttl_seconds }` in `core/config.rs`; `cache` field on `RouterConfig`
- [x] T-03 — `cache_hit: bool` added to `SearchResponse`
- [x] T-04 — `crates/router/src/cache.rs`: `QueryCacheKey` (sorted filters, excludes include_docs), `CachedResponse`, `QueryCache` wrapper with metrics
- [x] T-05 — `Router::with_cache` constructor; cache lookup before scatter; store after availability filter (full candidate list, pre-pagination); `respond_from_cache` + `build_hits` helpers
- [x] T-06 — `main.rs`: `Router::with_cache(&config.router.cache)` in router mode
- [x] T-07 — Unit tests: hit/miss; TTL disabled returns None; include_docs not in key; filter order independent; different zone = different key
- [x] T-08 — Integration tests: `test_cache_hit_skips_scatter`; `test_cache_different_zone_is_miss`
