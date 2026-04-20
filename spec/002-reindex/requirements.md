# Requirements: In-Place Reindex

## Feature Overview

When the index format or schema changes (e.g. new posting list format, language switch, field boost changes), operators need a way to rebuild the index from stored documents without re-ingesting data from an external source. The reindex operation reads all stored documents from each shard's document store, writes a fresh set of segments using the current schema and format, and atomically replaces the old segments — all without downtime.

## User Stories

### US-1: Reindex after schema change
**As an** operator  
**I want to** trigger a reindex on all shards via a single API call  
**So that** all documents are re-analyzed and re-scored under the current schema without re-ingesting from the source system

**Acceptance Criteria:**
- A single `POST /v1/admin/reindex` call triggers reindex on all shards
- The shard reads every stored document, re-analyzes it with the current schema, and writes a new segment
- Old segments are replaced atomically once the new segment is fully written
- Search remains available during reindex (reads from old segments until swap)
- The endpoint returns once all shards have completed

### US-2: Reindex a single shard
**As an** operator  
**I want to** reindex one specific shard without affecting others  
**So that** I can test or roll back incrementally

**Acceptance Criteria:**
- A `POST /v1/admin/reindex?shard=N` parameter targets a single shard
- Other shards continue serving traffic normally

### US-3: Progress visibility
**As an** operator  
**I want to** know how far along reindex is  
**So that** I can estimate completion time for large indexes

**Acceptance Criteria:**
- The response body includes `{ docs_reindexed, total_docs, shards_done, total_shards }`
- For long-running reindexes, the operator can poll `/v1/stats` to observe segment counts evolving

## Functional Requirements

| ID | Requirement |
|----|------------|
| FR-01 | `POST /v1/admin/reindex` triggers full reindex across all shards |
| FR-02 | Each shard reads all live documents from its document store |
| FR-03 | Documents are re-analyzed using the current in-memory schema |
| FR-04 | A new segment is written with the current posting list format (Block-Max WAND blocks) |
| FR-05 | Old segments are atomically swapped out after the new segment is written |
| FR-06 | WAL is truncated after successful reindex (old WAL entries are now redundant) |
| FR-07 | Search continues to serve from old segments during reindex |
| FR-08 | Reindex is idempotent — running it twice leaves the index in the same state |
| FR-09 | Response includes per-shard doc counts and total elapsed time |
| FR-10 | Write buffer is flushed to a segment before reindex reads begin (no in-flight writes lost) |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | Reindex throughput | >= 50,000 docs/sec per shard (matches bulk indexing throughput) |
| NFR-02 | Search latency during reindex | No degradation (reads from old segments) |
| NFR-03 | Atomicity | Old segments never removed until new segment is fully flushed to disk |
| NFR-04 | No data loss | All documents present before reindex are present after |

## Out of Scope

- Incremental / partial reindex (only changed documents)
- Cross-shard document migration (resharding)
- Online schema migration with dual-write
- Reindex triggered automatically on schema change detection
