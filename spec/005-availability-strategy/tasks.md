# Tasks: Pluggeable Availability Strategy

## Status: Implemented — pending deploy

- [x] T-01 — `destination_zone: Option<String>` added to `SearchRequest`
- [x] T-02 — `AvailabilityConfig`, `AvailabilityStrategyType`, `StaticAvailabilityConfig`, `RegionStockConfig` added to `core/config.rs`; `availability` field on `RouterConfig`
- [x] T-03 — `reqwest` (rustls-tls, no OpenSSL) added to workspace and `search-router`
- [x] T-04 — `crates/router/src/availability.rs`: `AvailabilityStrategy` trait, `NoopStrategy`, `StaticAvailabilityStrategy`, `RegionStockStrategy`, `build_strategy` factory
- [x] T-05 — Router: `strategy` field, scatter limit inflated by `candidate_multiplier()`, candidate docs fetched before re-rank block (shared by ranker + strategy), filter applied post-rerank pre-pagination
- [x] T-06 — `main.rs`: `build_strategy(&config.router.availability)` passed to `Router::new()`
- [x] T-07 — Unit tests: noop passes all; static filters zero-stock; default_available true/false; region_stock fail-open
- [x] T-08 — Integration test: `test_search_with_destination_zone` — router with StaticAvailabilityStrategy, only in-stock doc returned
