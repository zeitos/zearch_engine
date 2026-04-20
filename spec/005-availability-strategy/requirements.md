# Requirements: Pluggeable Availability Strategy

## Feature Overview

Post-retrieval availability filtering: after the shard layer returns a pool of scored
candidates, the router applies an **availability strategy** that removes items that are
out-of-stock or unavailable for a given `destination_zone` before paginating and
returning results to the client.

The strategy is pluggeable — operators pick one at deploy time via config, without
recompiling or redeploying the core engine.

## User Stories

### US-1: Zone-aware availability filtering
**As a** search client serving a user in Santa Cruz  
**I want** results that exclude items with no stock in that zone  
**So that** the user never sees products they can't buy

**Acceptance Criteria:**
- Request includes `destination_zone: "santa_cruz"`
- Items with `attributes["stock_santa_cruz"] == 0` (or missing and `default_available: false`) are excluded
- Items where the attribute is absent and `default_available: true` are included

### US-2: No-op default (backward compatible)
**As a** client that does not supply `destination_zone`  
**I want** search results to be identical to today  
**So that** existing integrations are not broken

**Acceptance Criteria:**
- If `destination_zone` is absent or empty, no availability filtering is applied
- The `NoopStrategy` (default) passes all candidates through unchanged

### US-3: External stock service integration
**As an** operator with a dedicated stock microservice  
**I want** the engine to call that service post-retrieval  
**So that** availability data stays in one authoritative source without duplicating it into doc attributes

**Acceptance Criteria:**
- `RegionStockStrategy` calls `GET {base_url}/availability?zone=X&doc_ids=1,2,3`
- Expects `{ "available_ids": [1, 3] }` response
- If the call fails or times out, all candidates pass through (fail-open)
- Configurable `timeout_ms` (default: 50ms) to stay within query budget

### US-4: Static doc-attribute strategy (no external service)
**As an** operator who stores stock per zone in document attributes  
**I want** availability filtering driven entirely by indexed data  
**So that** I get zone filtering with zero external dependencies

**Acceptance Criteria:**
- `StaticAvailabilityStrategy` checks `doc.attributes["{prefix}{zone}"]`
- Numeric value ≥ `min_stock` (default: 1) → available
- Attribute absent → governed by `default_available` (default: true)
- No network calls; runs in microseconds per candidate

## Functional Requirements

| ID | Requirement |
|----|------------|
| FR-01 | `SearchRequest` gains an optional `destination_zone` field |
| FR-02 | When `destination_zone` is absent or empty, no filtering is performed |
| FR-03 | The router holds one `AvailabilityStrategy` instance, selected at startup from config |
| FR-04 | Strategy is applied after re-ranking, before pagination |
| FR-05 | Strategies report a `candidate_multiplier()` that inflates the scatter fetch size to compensate for expected filtering |
| FR-06 | `NoopStrategy` — passes all candidates unchanged; multiplier = 1 |
| FR-07 | `StaticAvailabilityStrategy` — filters by `attributes["{prefix}{zone}"]`; multiplier = 3 |
| FR-08 | `RegionStockStrategy` — calls external HTTP service; multiplier = 3; fail-open |
| FR-09 | Strategy type and its parameters are configured in `router.availability` config block |
| FR-10 | Strategy config is optional; absence or `strategy: noop` maps to `NoopStrategy` |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | `StaticAvailabilityStrategy` overhead per query | < 1ms for 1,000 candidates |
| NFR-02 | `RegionStockStrategy` timeout | Configurable; default 50ms |
| NFR-03 | Fail-open on stock service error | No query failure; full candidate set passes through |
| NFR-04 | Zero breaking changes to existing queries without `destination_zone` | 100% backward compatible |

## Constraints

- Availability filtering runs at the **router layer only** — shards are unaware of zones
- Candidate pool for filtering is bounded by `ranker_candidates × candidate_multiplier()`
- The stock service protocol is fixed (`GET /availability?zone=&doc_ids=`); adapters for other protocols are out of scope
- WASM-based custom strategies are **out of scope** for this version (planned for 006)

## Out of Scope

- Per-seller availability strategies
- Automatic stock attribute sync from external catalogue
- WASM plugin loading
- Multi-zone queries (single `destination_zone` per request)
