# Requirements: Autocomplete / Suggest

## Feature Overview

A prefix-based suggestion endpoint backed by an **independent suggest cluster** —
separate pods, separate StatefulSets, separate SLA from the main search cluster.
The suggest index is eventually consistent and lossy by design: writes are
best-effort, there is no WAL, and a suggest shard that misses writes will diverge
silently. This is an explicit, documented trade-off, not a bug.

The long-term feeding model is two independent Kafka consumers (one for search,
one for suggest). Today the router fans out writes to both clusters in parallel,
fire-and-forget for suggest.

## Consistency Model

| Cluster | Durability | Consistency | On shard crash |
|---------|-----------|-------------|----------------|
| Search  | WAL + async replication | Eventual + durable | Replays WAL on restart |
| Suggest | None (best-effort) | Eventual + lossy | Loses recent writes — acceptable |

Clients MUST NOT rely on suggest results being complete or up-to-date.
A `"consistency": "eventual"` field in the response documents this contract.

## User Stories

### US-1: Real-time prefix suggestions
**As a** search client rendering a search box  
**I want** to call `GET /v1/suggest?q=wire&limit=5` and receive ordered completions  
**So that** I can show users relevant suggestions before they finish typing

**Acceptance Criteria:**
- Prefix `"wire"` returns terms like `["wireless", "wired"]` ranked by doc frequency
- Response time < 20ms p99
- Returns at most `limit` suggestions (default 5, max 20)

### US-2: Independent SLA from search
**As an** operator  
**I want** suggest pods to be scaled and deployed independently from search shards  
**So that** a suggest cluster overload or deployment does not affect search latency

**Acceptance Criteria:**
- Suggest cluster has its own StatefulSet and headless service
- Router timeout for suggest fan-out is independent from search query timeout
- If all suggest shards are unavailable, the endpoint returns 503 without affecting `/v1/search`

### US-3: Fire-and-forget writes
**As an** operator  
**I want** document indexing to not slow down because a suggest shard is slow  
**So that** write throughput is not coupled to suggest cluster health

**Acceptance Criteria:**
- Router sends index/bulk writes to suggest shards with a short timeout (default 200ms)
- Write errors to suggest shards are logged but do not propagate to the caller
- Suggest shards do not participate in flush acknowledgement

### US-4: Explicit eventual consistency
**As a** client developer  
**I want** the API to be explicit about consistency guarantees  
**So that** I don't build features that assume suggest results are complete

**Acceptance Criteria:**
- Response includes `"consistency": "eventual"`
- README documents the lossy nature of the suggest index

## Functional Requirements

| ID | Requirement |
|----|------------|
| FR-01 | New endpoint `GET /v1/suggest?q={prefix}&limit={n}&field={f}` |
| FR-02 | Returns `{ "suggestions": [...], "consistency": "eventual", "took_ms": N }` |
| FR-03 | Prefix matching is case-insensitive |
| FR-04 | Terms scored by normalized document frequency (`doc_freq / max_doc_freq`) |
| FR-05 | Router fans out to all suggest shards, merges by max-score per term, returns top-`limit` |
| FR-06 | Writes (index, bulk) are forwarded to suggest shards fire-and-forget with a short timeout |
| FR-07 | Suggest shards run the same binary as search shards, in a new `mode: suggest` |
| FR-08 | Suggest shards maintain a `SuggestIndex` (sorted Vec of terms + scores); no WAL |
| FR-09 | `SuggestIndex` is rebuilt from the in-memory write buffer + flushed segments on each flush |
| FR-10 | `SuggestConfig { min_doc_frequency, max_terms_per_shard, write_timeout_ms }` in config |
| FR-11 | Router config gains a `suggest_shards` list (parallel to `shards`); if empty, suggest is disabled |
| FR-12 | If `q` is empty, return empty suggestions |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | Suggest p99 latency | < 20ms |
| NFR-02 | Write fan-out overhead on index path | < 1ms (async, fire-and-forget) |
| NFR-03 | Suggest shard memory overhead vs search shard | < 20% of equivalent search shard |
| NFR-04 | Search cluster unaffected by suggest cluster failure | 100% — no shared state |

## Constraints

- Suggest shards have **no WAL** — this is intentional
- Suggest shards receive writes from the router only (no replication, no replicas in v1)
- Only single-term suggestions in v1 — no phrase completions
- No typo tolerance in v1 — exact prefix match only
- Suggest cluster does not participate in stats, reindex, or flush admin endpoints

## Out of Scope

- Kafka consumer integration (future — today router does the fan-out)
- Phrase / n-gram completions
- Fuzzy prefix matching
- Per-user personalized suggestions
- Suggest replicas / replication
