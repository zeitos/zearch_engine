# Requirements: Product Search Engine

## Feature Overview

A high-performance, distributed search engine written in Rust, purpose-built for e-commerce product discovery. The engine indexes up to 1 billion products with real-time update support, provides full-text search with typo tolerance and plural handling, filtering, and count aggregations. It is designed to be horizontally scalable via sharding.

## User Stories

### US-1: Full-text product search
**As a** client application  
**I want to** search products by keywords against titles and descriptions  
**So that** users find relevant products quickly  

**Acceptance Criteria:**
- Queries match against product title and description fields
- Title matches are weighted higher than description matches
- Results are ranked by relevance score
- Search latency stays under target even at 1B documents

### US-2: Typo-tolerant search
**As a** client application  
**I want to** get relevant results even when users misspell search terms  
**So that** typos don't result in zero results  

**Acceptance Criteria:**
- Single-character typos return correct matches (e.g., "samsnug" -> "samsung")
- Edit distance is configurable (default: 1 for short words, 2 for longer words)
- Typo tolerance does not significantly degrade latency

### US-3: Plural and stemming support
**As a** client application  
**I want to** match products regardless of singular/plural form  
**So that** "shoe" and "shoes" return the same results  

**Acceptance Criteria:**
- Singular and plural forms are treated as equivalent at index and query time
- Common inflections are handled for English, Spanish, and Portuguese
- Stemming does not produce false positives on unrelated words

### US-4: Filtered search
**As a** client application  
**I want to** filter search results by product attributes  
**So that** users can narrow results to specific categories, price ranges, etc.  

**Acceptance Criteria:**
- Supports equality filters (e.g., `category = "electronics"`)
- Supports range filters (e.g., `price >= 10 AND price <= 100`)
- Supports multi-value filters (e.g., `brand IN ["nike", "adidas"]`)
- Filters are applied before scoring/ranking
- Filters can be combined with full-text search or used standalone

### US-5: Count aggregations
**As a** client application  
**I want to** get count aggregations on product attributes alongside search results  
**So that** I can display facet counts (e.g., "Electronics (1,234)")  

**Acceptance Criteria:**
- Returns per-value document counts for specified fields
- Aggregations respect active filters and query
- Aggregation computation does not significantly increase query latency

### US-6: Real-time indexing
**As a** client application  
**I want to** index, update, and delete products and have changes reflected immediately  
**So that** search results stay in sync with the product catalog  

**Acceptance Criteria:**
- Newly indexed documents are searchable within 1 second
- Updates to existing documents are reflected within 1 second
- Deletes remove documents from results within 1 second
- Real-time updates do not block or degrade ongoing search queries

### US-7: Horizontal scaling via sharding
**As an** operator  
**I want to** distribute the index across multiple shards  
**So that** the engine scales to 1B+ documents  

**Acceptance Criteria:**
- Index can be split across N shards
- Queries fan out to all shards and merge results
- Shards run on separate nodes, communicating via gRPC
- Adding shards allows scaling storage and throughput

### US-8: Pluggable external ranker
**As a** team operating the search engine  
**I want to** plug in an external ranking service that re-ranks results after BM25 retrieval  
**So that** we can apply ML models, business rules, or personalization without modifying the engine  

**Acceptance Criteria:**
- Engine supports an optional re-ranking phase after BM25 scoring
- External ranker is called via gRPC with the top-N candidates and the original query
- Ranker must respond within 30ms; on timeout, engine falls back to BM25 order
- Ranker is optional — if not configured, BM25 scores are final
- Response indicates whether re-ranking was applied or fell back to BM25
- WASM-based in-process rankers are supported as an alternative to gRPC

## Functional Requirements

### P0 — Must Have
| ID | Requirement |
|----|------------|
| FR-01 | Full-text search over product title and description fields |
| FR-02 | Relevance ranking with configurable field boosting (title > description) |
| FR-03 | Inverted index with tokenization, lowercasing, and normalization |
| FR-04 | Typo tolerance via edit-distance matching (Levenshtein / Damerau-Levenshtein) |
| FR-05 | Plural/stemming support at index and query time (English, Spanish, Portuguese) |
| FR-06 | Equality, range, and multi-value attribute filters |
| FR-07 | Count aggregations per field value |
| FR-08 | Real-time indexing — documents searchable within 1s of write |
| FR-09 | Document updates and deletes with real-time visibility |
| FR-10 | Sharded index architecture — distribute data across N shards on separate nodes |
| FR-11 | Scatter-gather query execution across shards via gRPC with result merging |
| FR-11b | Deployable as a distributed cluster on Kubernetes (router Deployment + shard StatefulSet) |
| FR-12 | Pagination support (offset/limit) |
| FR-13 | REST or gRPC API for search, index, update, and delete operations |
| FR-14 | Product schema: `id`, `title`, `description`, `price`, `category`, and extensible attributes |
| FR-14b | Pluggable two-phase ranking: BM25 retrieval → optional external re-ranker (gRPC or WASM) with 30ms timeout and BM25 fallback |

### P1 — Should Have
| ID | Requirement |
|----|------------|
| FR-15 | Prefix matching (autocomplete-style queries) |
| FR-16 | Configurable tokenizers and analyzers per field |
| FR-17 | Shard replication for fault tolerance |
| FR-18 | Bulk indexing API for initial data load |
| FR-19 | Query-time field boosting |
| FR-20 | Highlighted matched terms in results |
| FR-21 | Sort by relevance, price, or other numeric fields |

### P2 — Nice to Have (Future Roadmap)

> **Constraint:** The architecture and design must not preclude any P2 feature. These are deferred from v1 but guaranteed to be implementable without major redesign.

| ID | Requirement |
|----|------------|
| FR-22 | Synonym support (configurable synonym maps) |
| FR-23 | Stop word removal (configurable) |
| FR-24 | Shard rebalancing / dynamic resharding |
| FR-25 | Query caching layer |
| FR-26 | Language detection to auto-select analyzer |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | Search latency (p99) | < 50ms at 1M docs per shard |
| NFR-02 | Search latency (p99) | < 200ms at full scale (1B docs, sharded) |
| NFR-03 | Indexing throughput | >= 50,000 documents/sec (bulk) |
| NFR-04 | Real-time index freshness | < 1 second from write to searchable |
| NFR-05 | Index capacity | 1,000,000,000 (1B) products |
| NFR-06 | Concurrent query throughput | >= 1,000 queries/sec per node |
| NFR-07 | Memory efficiency | Reasonable RAM usage — index primarily on disk with hot caches |
| NFR-08 | Written in Rust | Entire engine codebase in Rust |
| NFR-09 | Test coverage | >= 80% for core search and indexing logic |
| NFR-10 | Crash resilience | Index is durable — recoverable after process crash without data loss |

## Constraints and Assumptions

### Constraints
- The engine must be implemented in Rust
- Must handle 1 billion documents across shards without degradation
- Must support real-time writes — no batch-only indexing model
- Sharding is a first-class architectural concern, not bolted on later
- Architecture must leave clear extension points for all P2 features — none may be blocked by design decisions made for v1

### Assumptions
- Products have a stable schema with known fields (title, description, price, category) plus dynamic attributes
- Supported languages for v1: English, Spanish, and Portuguese (stemming, plurals, typo handling tuned for all three)
- Clients communicate over the network via REST or gRPC
- The engine ships with Kubernetes manifests for distributed deployment (router Deployment + shard StatefulSet)
- Persistent storage uses local disk (SSDs assumed)

## Out of Scope

- Client SDKs or CLI tools
- Admin UI or dashboard
- Multi-tenancy
- Authentication / authorization
- Machine-learned ranking models
- Image or vector search
- Shopping cart, checkout, or any e-commerce application logic
- Helm charts (plain K8s manifests are in scope, Helm is not)
- Monitoring / observability stack (the engine exposes metrics, but dashboards are external)

## Success Metrics

| Metric | Target | How to Measure |
|--------|--------|----------------|
| Search relevance | Top-3 results contain expected product for > 90% of test queries | Evaluation on curated query set |
| Typo recovery | Correct product appears in results for > 95% of single-typo queries | Automated test suite with typo variants |
| Search latency (p99) | < 200ms at 1B scale | Load testing with realistic query distribution |
| Indexing throughput | >= 50K docs/sec bulk, real-time < 1s | Benchmark suite |
| Zero-result rate | < 5% of realistic queries | Test query log analysis |
| Crash recovery | No data loss after kill -9 | Chaos test: kill process, restart, verify doc count |
| Shard scalability | Linear throughput increase with added shards | Benchmark at 1, 4, 16, 64 shards |
