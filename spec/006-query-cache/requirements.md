# Requirements: Query Cache

## Feature Overview

An in-process LRU cache at the router level that stores search results keyed by the
full query parameters. Cache hits skip the scatter/merge/rerank/availability pipeline
entirely, reducing latency and shard load for repeated or popular queries.

## User Stories

### US-1: Popular queries served from cache
**As** the router  
**I want** to return cached results for queries seen recently  
**So that** repeated queries (e.g. trending searches) don't fan-out to every shard on every request

**Acceptance Criteria:**
- Identical requests within `ttl_seconds` return the cached response
- Cache hit is measurably faster than a shard fan-out
- `cache_hit: bool` field in the response indicates whether the result came from cache

### US-2: Cache is invisible to the caller
**As** a search client  
**I want** cached and non-cached responses to be identical  
**So that** I don't need to know or care about the cache

**Acceptance Criteria:**
- Response structure is the same regardless of cache hit/miss
- `include_docs: true` works correctly on cache hits (docs fetched after hit)
- `destination_zone` is part of the cache key — different zones get different entries

### US-3: Operator control
**As** an operator  
**I want** to tune or disable the cache via config  
**So that** I can trade memory for hit rate or turn it off entirely during debugging

**Acceptance Criteria:**
- `router.cache.enabled: false` disables caching completely (default)
- `max_entries` controls memory footprint
- `ttl_seconds` controls staleness window

## Functional Requirements

| ID | Requirement |
|----|------------|
| FR-01 | Cache is in-process at the router; no external dependency |
| FR-02 | Cache key covers: query, filters, aggregations, sort, offset, limit, typo_tolerance, language, destination_zone |
| FR-03 | `include_docs` is NOT part of the cache key — the same entry serves both |
| FR-04 | Cached value is the post-rerank, post-availability-filter candidate list + metadata |
| FR-05 | On cache hit: slice by offset/limit, fetch docs if `include_docs: true`, return |
| FR-06 | On cache miss: run full pipeline, store result, return |
| FR-07 | TTL-based expiry only — no explicit invalidation on write |
| FR-08 | `cache_hit: bool` added to `SearchResponse` |
| FR-09 | Cache disabled by default (`enabled: false`) |
| FR-10 | Metrics: `search_cache_hits_total`, `search_cache_misses_total`, `search_cache_size` |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | Cache hit latency overhead | < 1ms |
| NFR-02 | Memory per cache entry (200 candidates) | ~5 KB |
| NFR-03 | 10,000 entries total cache memory | ~50 MB |
| NFR-04 | No cache hit on writes — write path unchanged | — |

## Constraints

- In-process only — no Redis, no Memcached
- TTL invalidation only — writes do not invalidate related cache entries (acceptable staleness)
- Cache is per-router-instance — two router replicas have independent caches
- `destination_zone` in key means zone fragmentation; accepted trade-off for correctness

## Out of Scope

- Distributed / shared cache across router replicas
- Write-through or write-invalidation
- Partial cache (caching only the scored IDs, not aggregations)
- Cache warming
