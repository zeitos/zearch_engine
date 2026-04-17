# Technical Design: Product Search Engine

## 1. Architecture Overview

The engine follows a **shared-nothing, sharded architecture** where each shard is an independent search node owning a partition of the data. A lightweight **router** layer handles query fan-out, result merging, optional two-phase re-ranking, and client communication.

```
                         ┌─────────────────┐
                         │   Client (HTTP)  │
                         └────────┬─────────┘
                                  │
                                  ▼
                         ┌─────────────────┐
                         │     Router       │
                         │  (query parsing, │
                         │   fan-out,       │
                         │   merge, API)    │
                         └───┬───┬───┬──────┘
                             │   │   │
                 ┌───────────┘   │   └───────────┐
                 ▼               ▼               ▼
          ┌────────────┐ ┌────────────┐ ┌────────────┐
          │  Shard 0   │ │  Shard 1   │ │  Shard N   │
          │            │ │            │ │            │
          │ ┌────────┐ │ │ ┌────────┐ │ │ ┌────────┐ │
          │ │Inverted│ │ │ │Inverted│ │ │ │Inverted│ │
          │ │ Index  │ │ │ │ Index  │ │ │ │ Index  │ │
          │ ├────────┤ │ │ ├────────┤ │ │ ├────────┤ │
          │ │ Column │ │ │ │ Column │ │ │ │ Column │ │
          │ │ Store  │ │ │ │ Store  │ │ │ │ Store  │ │
          │ ├────────┤ │ │ ├────────┤ │ │ ├────────┤ │
          │ │  Doc   │ │ │ │  Doc   │ │ │ │  Doc   │ │
          │ │ Store  │ │ │ │ Store  │ │ │ │ Store  │ │
          │ └────────┘ │ │ └────────┘ │ │ └────────┘ │
          │  [WAL]     │ │  [WAL]     │ │  [WAL]     │
          └────────────┘ └────────────┘ └────────────┘
```

### Component Roles

| Component | Responsibility |
|-----------|---------------|
| **Router** | HTTP API, query parsing, scatter-gather across shards, result merging, optional re-ranking, pagination |
| **Shard** | Owns a partition of documents. Contains inverted index, column store, document store, and WAL |
| **Inverted Index** | Term-to-posting-list mapping for full-text search |
| **Column Store** | Per-field columnar data for filters and aggregations |
| **Document Store** | Full document storage for returning results |
| **WAL** | Write-ahead log for crash resilience and real-time durability |

### Process Model

The engine runs as a **distributed cluster** of two node types:

- **Router nodes** — stateless, handle HTTP API, fan-out queries to shards via gRPC, merge results
- **Shard nodes** — stateful, each owns one or more shard partitions with local disk storage

```
┌────────────────┐  ┌────────────────┐
│  Router Node 0 │  │  Router Node 1 │   (stateless, N replicas)
│  (HTTP + gRPC  │  │  (HTTP + gRPC  │
│   client)      │  │   client)      │
└───┬──┬──┬──────┘  └───┬──┬──┬──────┘
    │  │  │             │  │  │
    │  │  └─────┐  ┌────┘  │  │        gRPC
    │  │        │  │       │  │
┌───▼──▼───┐ ┌──▼──▼──┐ ┌─▼──▼────┐
│ Shard    │ │ Shard  │ │ Shard   │   (stateful, one per shard)
│ Node 0   │ │ Node 1 │ │ Node N  │
│ (gRPC    │ │ (gRPC  │ │ (gRPC   │
│  server) │ │ server)│ │ server) │
│ [disk]   │ │ [disk] │ │ [disk]  │
└──────────┘ └────────┘ └─────────┘
```

The single binary supports both roles via CLI flag (`--mode router` or `--mode shard`). For development and testing, `--mode standalone` runs a router and N shards in a single process.

## 2. Technology Stack

| Layer | Choice | Rationale |
|-------|--------|-----------|
| Language | **Rust** (2021 edition) | Required by constraints. Zero-cost abstractions, memory safety, great concurrency |
| Async runtime | **Tokio** | Industry standard for async Rust. Multi-threaded work-stealing for I/O and CPU overlap |
| HTTP framework | **Axum** | Built on Tokio/Hyper, ergonomic, high performance |
| Serialization (API) | **serde + serde_json** | Standard Rust JSON handling |
| Serialization (disk) | **rkyv** (zero-copy) or **bincode** | Fast serialization for on-disk structures, minimal decode overhead |
| On-disk storage | **Custom segment files** | Memory-mapped segment files for inverted index, column store, and doc store |
| Stemming | **rust-stemmers** (Snowball) | Supports English, Spanish, Portuguese out of the box |
| Fuzzy matching | **Custom Levenshtein automaton** | Built on FST for efficient edit-distance traversal |
| FST (finite state transducer) | **fst** crate | Compact, ordered term dictionary with fast prefix and fuzzy lookups |
| Memory mapping | **memmap2** | Zero-copy access to on-disk index segments |
| Inter-node RPC | **tonic** (gRPC) | High-performance async gRPC for Rust, built on Tokio. Router↔Shard communication |
| Protobuf | **prost** | Code generation for gRPC service definitions |
| Service discovery | **K8s headless Service** | Shard nodes discovered via DNS SRV records or endpoint API |
| WASM runtime | **wasmtime** | In-process sandboxed execution for WASM re-ranker plugins |
| Container | **Docker** (multi-stage build) | Single binary image, ~20MB scratch-based |
| Orchestration | **Kubernetes** | StatefulSet for shard nodes, Deployment for router nodes |
| Testing | **cargo test** + **criterion** | Unit/integration tests + benchmarks |
| Build | **Cargo workspaces** | Multi-crate project structure |

## 3. Data Model

### 3.1 Document Schema

```rust
struct Document {
    id: u64,                            // unique product ID
    title: String,                      // full-text indexed, boosted
    description: String,                // full-text indexed
    price: f64,                         // filterable, sortable
    category: String,                   // filterable, aggregatable
    attributes: HashMap<String, Value>, // extensible key-value pairs
}

enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    StringArray(Vec<String>),
}
```

### 3.2 Index Schema Configuration

```rust
struct IndexSchema {
    fields: Vec<FieldConfig>,
}

struct FieldConfig {
    name: String,
    field_type: FieldType,
    indexed: bool,          // included in inverted index
    stored: bool,           // stored in doc store for retrieval
    filterable: bool,       // stored in column store
    aggregatable: bool,     // available for count aggregations
    boost: f32,             // relevance weight (default 1.0)
    analyzer: AnalyzerType, // text analysis pipeline
}

enum FieldType {
    Text,       // analyzed, tokenized
    Keyword,    // exact match only, not tokenized
    Numeric,    // f64, for range filters and sorting
    Boolean,
}

enum AnalyzerType {
    Standard { language: Language },
    Keyword,  // no analysis, stored as-is
    // P2 extension point: Custom(Vec<TokenFilter>)
}

enum Language {
    English,
    Spanish,
    Portuguese,
}
```

### 3.3 On-Disk Layout

Each shard is a directory containing **immutable segments** plus an **active write buffer**:

```
data/
  shard-0/
    wal/                    # write-ahead log
      000001.wal
      000002.wal
    segments/
      seg-0001/
        inverted.idx        # term dict (FST) + posting lists
        columns.col         # columnar field data
        docs.store          # stored document fields
        meta.json           # segment metadata (doc count, term count, etc.)
      seg-0002/
        ...
    active/                 # current write buffer (not yet segment)
      buffer.bin
    shard_meta.json         # shard config, segment list, deletion bitmap
  shard-1/
    ...
```

## 4. Core Engine Design

### 4.1 Text Analysis Pipeline

```
Input text
    │
    ▼
┌──────────────┐
│  Tokenizer   │  Unicode-aware word boundary splitting
└──────┬───────┘
       ▼
┌──────────────┐
│  Lowercase   │  Unicode lowercasing
└──────┬───────┘
       ▼
┌──────────────┐
│  Stemmer     │  Snowball stemmer (EN/ES/PT based on field config)
└──────┬───────┘
       ▼
┌──────────────┐
│  (P2: Stop   │  Extension point — not implemented in v1
│   words)     │
└──────┬───────┘
       ▼
┌──────────────┐
│  (P2: Syn-   │  Extension point — not implemented in v1
│   onyms)     │
└──────┬───────┘
       ▼
   Token stream
```

The pipeline is a `Vec<Box<dyn TokenFilter>>` — each filter transforms the token stream. This trait-based design makes P2 additions (stop words, synonyms, language detection) a matter of implementing a new `TokenFilter` and inserting it into the chain.

### 4.2 Inverted Index

**Term Dictionary:** An FST (finite state transducer) maps terms to term IDs. The FST supports:
- Exact lookups: O(term_length)
- Prefix enumeration: for autocomplete (FR-15)
- Levenshtein automaton intersection: for typo tolerance (FR-04)

**Posting Lists:** Each term ID maps to a compressed posting list (doc IDs + term frequency + positions). Compression uses **PFOR-delta** (patched frame-of-reference) for doc IDs and frequencies.

```
Term Dict (FST)             Posting Lists
┌──────────────┐            ┌────────────────────────────┐
│ "laptop" → 42│───────────▶│ [doc:7, tf:2] [doc:91, tf:1] ... │
│ "phone"  → 87│───────────▶│ [doc:3, tf:1] [doc:15, tf:3] ... │
│ ...          │            │ ...                        │
└──────────────┘            └────────────────────────────┘
```

**Typo Tolerance:** At query time, build a Levenshtein automaton for each query term and intersect it with the FST. This efficiently enumerates all terms within edit distance N without scanning the full dictionary.

```rust
// Pseudocode for fuzzy search
fn fuzzy_lookup(fst: &Fst, term: &str, max_distance: u8) -> Vec<(String, TermId)> {
    let automaton = LevenshteinAutomaton::new(term, max_distance);
    fst.search(automaton).collect()
}
```

Edit distance defaults:
- Term length <= 4: distance 1
- Term length >= 5: distance 2

### 4.3 Column Store

Columnar storage for filterable/aggregatable fields. Each field is stored as a separate column file:

| Field Type | Column Format |
|-----------|---------------|
| Keyword | Dictionary-encoded u32 ordinals + ordinal-to-value table |
| Numeric | Packed f64 array (or bit-packed integers if values allow) |
| Boolean | Bitset |

**Filters** operate directly on columns, producing a `BitSet` of matching doc IDs. Multiple filters are combined with bitwise AND/OR.

**Aggregations** iterate over the column for matching docs, counting occurrences per ordinal value. Dictionary encoding makes this a fast u32 histogram.

### 4.4 Document Store

Stores the original field values for result retrieval. Documents are serialized with bincode/rkyv and stored in blocks. A doc-ID-to-offset index enables direct access.

Deleted documents are tracked via a **deletion bitmap** (roaring bitmap per segment). Deleted docs are excluded at query time and physically removed during segment merges.

### 4.5 Segment Architecture

The index uses an **LSM-inspired segment model**:

1. **Write buffer:** Incoming documents accumulate in an in-memory buffer
2. **Flush:** When the buffer reaches a size threshold (e.g., 64MB), it is written as a new immutable segment on disk
3. **Search:** Queries search all segments and merge results
4. **Merge:** Background process merges small segments into larger ones (tiered merge policy), reclaiming space from deleted docs

```
Write path:
  Document ──▶ WAL ──▶ Write Buffer ──▶ [flush] ──▶ Immutable Segment

Read path:
  Query ──▶ Search all segments ──▶ Merge results ──▶ Apply deletions ──▶ Return
```

This provides the real-time indexing guarantee (FR-08): once a document is in the write buffer and WAL, it is searchable. No explicit "commit" or "refresh" is needed.

### 4.6 Write-Ahead Log (WAL)

Every write operation (index, update, delete) is appended to the WAL before being applied to the write buffer. On crash recovery:

1. Read the WAL from the last checkpoint
2. Replay operations into the write buffer
3. Resume normal operation

WAL is truncated after a segment flush (the flushed segment is the durable checkpoint).

## 5. Sharding Design

### 5.1 Shard Assignment

Documents are assigned to shards by consistent hashing on `document.id`:

```rust
fn shard_for_doc(doc_id: u64, num_shards: u32) -> u32 {
    // Jump consistent hash — uniform distribution, minimal reshuffling
    jump_consistent_hash(doc_id, num_shards)
}
```

### 5.2 Scatter-Gather Query Execution

```
┌──────────┐
│  Router   │
│           │
│ 1. Parse query
│ 2. Fan out to all shards (parallel)
│ 3. Each shard returns top-K + aggregations
│ 4. Merge top-K results across shards (top-N candidates)
│ 5. Merge aggregation counts
│ 6. [Optional] Send top-N to re-ranker (30ms timeout, BM25 fallback)
│ 7. Apply global pagination on final order
│ 8. Fetch full documents for final page
│           │
└──────────┘
```

Each shard returns:
- Top-K scored document IDs (with scores)
- Aggregation counts for requested fields
- Total hit count

The router merges using a **priority queue** across shard results, applies global offset/limit, then fetches full documents only for the final result set.

### 5.3 Shard Communication

Router↔Shard communication uses **gRPC** (tonic) in all deployment modes. Even in standalone mode, the router calls shards through the same gRPC interface for consistency.

**gRPC Service Definition:**

```protobuf
service ShardService {
    rpc Search (SearchRequest) returns (ShardSearchResponse);
    rpc GetDocs (GetDocsRequest) returns (GetDocsResponse);
    rpc Index (IndexRequest) returns (IndexResponse);
    rpc Delete (DeleteRequest) returns (DeleteResponse);
    rpc Stats (StatsRequest) returns (StatsResponse);
    rpc Health (HealthRequest) returns (HealthResponse);
}
```

The Rust-side trait mirrors this:

```rust
#[async_trait]
trait ShardClient {
    async fn search(&self, query: &SearchRequest) -> Result<ShardSearchResponse>;
    async fn get_docs(&self, doc_ids: &[u64]) -> Result<Vec<Document>>;
    async fn index(&self, doc: &Document) -> Result<IndexResponse>;
    async fn delete(&self, doc_id: u64) -> Result<DeleteResponse>;
}
```

`RemoteShardClient` wraps a tonic gRPC client. In standalone mode, `LocalShardClient` wraps a direct function call but still serializes/deserializes through the same protobuf types.

### 5.4 Cluster Topology and Discovery

The router must know which shard nodes exist and which shard partitions they own. In v0 this is handled via:

1. **Static configuration:** A config file or env vars listing shard endpoints (`SHARD_ENDPOINTS=shard-0:9001,shard-1:9001,...`)
2. **K8s headless Service:** Router discovers shard pods via DNS. Each shard pod is a member of a StatefulSet with stable network identity (`shard-0.shards.default.svc.cluster.local`)

Shard assignment is deterministic: shard node `shard-N` owns partition N. The router computes `jump_consistent_hash(doc_id, num_shards)` to route writes to the correct shard.

## 6. Relevance Scoring

**BM25** is the scoring function, computed per-field with configurable boosting:

```
score(doc, query) = Σ for each field f:
    boost(f) × Σ for each query term t:
        BM25(t, doc.f)

BM25(t, d) = IDF(t) × (tf(t,d) × (k1 + 1)) / (tf(t,d) + k1 × (1 - b + b × |d| / avgdl))
```

Defaults: `k1 = 1.2`, `b = 0.75`

Field boosts (configurable):
- `title`: 2.0
- `description`: 1.0

For fuzzy matches, scores are penalized proportionally to edit distance.

## 7. Pluggable Re-Ranking

### 7.1 Two-Phase Ranking Architecture

The engine supports an optional second-phase re-ranking step after BM25 scoring. This allows external ML models, business rules, or personalization logic to influence result ordering without modifying the engine itself.

```
Router query flow:

  1. Scatter query to shards
  2. Merge BM25 results (top-N candidates, e.g., N=200)
  3. [Optional] Send top-N to re-ranker ──▶ Re-ranked order
  4. Apply pagination (offset/limit) on final order
  5. Return to client

              ┌──────────────────────────────────┐
              │            Router                 │
              │                                   │
              │  BM25 merge ──▶ top-200 candidates│
              │       │                           │
              │       ▼                           │
              │  ┌──────────────────┐             │
              │  │  Ranker trait    │             │
              │  │                  │             │
              │  │  ┌────────────┐  │             │
              │  │  │ NoopRanker │  │  (default)  │
              │  │  ├────────────┤  │             │
              │  │  │ GrpcRanker │  │  (external) │
              │  │  ├────────────┤  │             │
              │  │  │ WasmRanker │  │  (plugin)   │
              │  │  └────────────┘  │             │
              │  └──────────────────┘             │
              │       │                           │
              │       ▼                           │
              │  Re-ranked top-200 ──▶ paginate   │
              └──────────────────────────────────┘
```

### 7.2 Ranker Trait

```rust
#[async_trait]
pub trait Ranker: Send + Sync {
    /// Re-rank candidates. Returns doc IDs in new order.
    /// Input: query context + candidates with BM25 scores.
    /// Must complete within the configured timeout.
    async fn rerank(
        &self,
        query: &str,
        candidates: &[RankCandidate],
    ) -> Result<Vec<RankedResult>>;
}

pub struct RankCandidate {
    pub doc_id: u64,
    pub bm25_score: f32,
    pub title: String,
    pub category: String,
    pub price: f64,
    pub attributes: HashMap<String, Value>,
}

pub struct RankedResult {
    pub doc_id: u64,
    pub score: f32,
}
```

### 7.3 Ranker Implementations

**NoopRanker** (default): Returns candidates in BM25 order unchanged.

**GrpcRanker**: Calls an external gRPC service. The ranker service implements:

```protobuf
service RankerService {
    rpc Rerank (RerankRequest) returns (RerankResponse);
}

message RerankRequest {
    string query = 1;
    repeated RankCandidate candidates = 2;
}

message RerankResponse {
    repeated RankedResult results = 1;
}
```

The external service can be written in any language (Python with an ML model, Go with business rules, etc.). The gRPC callout runs within the K8s cluster so network overhead is <1ms.

**WasmRanker**: Loads a `.wasm` module at startup via wasmtime. The WASM module exports a `rerank` function. Runs in-process with microsecond invocation overhead, sandboxed. Useful for lightweight rankers (business rules, simple scoring adjustments) that don't need a full external service.

### 7.4 Timeout and Fallback

```rust
async fn rerank_with_fallback(
    ranker: &dyn Ranker,
    query: &str,
    candidates: &[RankCandidate],
    timeout: Duration, // default: 30ms
) -> (Vec<RankedResult>, bool /* was_reranked */) {
    match tokio::time::timeout(timeout, ranker.rerank(query, candidates)).await {
        Ok(Ok(results)) => (results, true),
        Ok(Err(_)) | Err(_) => {
            // Timeout or error: fall back to BM25 order
            let fallback = candidates.iter()
                .map(|c| RankedResult { doc_id: c.doc_id, score: c.bm25_score })
                .collect();
            (fallback, false)
        }
    }
}
```

The response includes a `reranked: bool` field so the client knows whether re-ranking was applied or BM25 fallback was used.

### 7.5 Configuration

```yaml
ranker:
  type: none | grpc | wasm       # default: none
  # gRPC ranker settings
  grpc_endpoint: "ranker:9002"
  # WASM ranker settings
  wasm_module: "/etc/search-engine/ranker.wasm"
  # Common settings
  timeout_ms: 30
  candidates: 200                # top-N candidates sent to ranker
```

### 7.6 Candidate Count

The number of candidates sent to the re-ranker (default: 200) is a tradeoff:
- More candidates → better re-ranking quality but higher ranker latency
- Fewer candidates → faster but risk missing relevant results that BM25 ranked lower
- The router always retrieves `max(candidates, offset + limit)` from shards to ensure enough data for re-ranking

## 8. API Design

### 8.1 Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/index` | Index or update a single document |
| `POST` | `/v1/index/bulk` | Bulk index documents (P1) |
| `DELETE` | `/v1/index/{id}` | Delete a document |
| `POST` | `/v1/search` | Search products |
| `GET` | `/v1/health` | Health check |
| `GET` | `/v1/stats` | Index statistics (doc count, shard info) |

### 8.2 Search Request

```json
POST /v1/search
{
    "query": "samsung galaxy",
    "filters": {
        "category": { "eq": "electronics" },
        "price": { "gte": 100, "lte": 1000 },
        "brand": { "in": ["samsung"] }
    },
    "aggregations": ["category", "brand"],
    "sort": { "field": "_score", "order": "desc" },
    "offset": 0,
    "limit": 20,
    "typo_tolerance": true,
    "language": "en"
}
```

### 8.3 Search Response

```json
{
    "hits": [
        {
            "id": 12345,
            "score": 8.72,
            "title": "Samsung Galaxy S24",
            "description": "Latest Samsung flagship...",
            "price": 799.99,
            "category": "electronics",
            "attributes": { "brand": "samsung", "color": "black" }
        }
    ],
    "total_hits": 1583,
    "aggregations": {
        "category": [
            { "value": "electronics", "count": 1200 },
            { "value": "accessories", "count": 383 }
        ],
        "brand": [
            { "value": "samsung", "count": 1583 }
        ]
    },
    "reranked": true,
    "took_ms": 12
}
```

### 8.4 Index Request

```json
POST /v1/index
{
    "id": 12345,
    "title": "Samsung Galaxy S24",
    "description": "Latest Samsung flagship smartphone with AI features",
    "price": 799.99,
    "category": "electronics",
    "attributes": {
        "brand": "samsung",
        "color": "black",
        "storage": "256GB"
    }
}
```

## 9. Crate Structure

```
search-engine/
├── Cargo.toml              # workspace root
├── Dockerfile              # multi-stage build
├── k8s/                    # Kubernetes manifests
│   ├── router-deployment.yaml
│   ├── shard-statefulset.yaml
│   ├── router-service.yaml
│   └── shard-headless-service.yaml
├── proto/
│   ├── shard.proto         # shard gRPC service definition
│   └── ranker.proto        # re-ranker gRPC service definition
├── crates/
│   ├── core/               # shared types, schema, config
│   │   └── src/lib.rs
│   ├── proto/              # generated gRPC code (prost + tonic)
│   │   ├── build.rs
│   │   └── src/lib.rs
│   ├── analysis/           # tokenizers, stemmers, analyzers
│   │   └── src/lib.rs
│   ├── index/              # inverted index, column store, doc store, segments
│   │   └── src/lib.rs
│   ├── query/              # query parsing, scoring (BM25), fuzzy matching
│   │   └── src/lib.rs
│   ├── shard/              # shard management, WAL, write buffer, flush, gRPC server
│   │   └── src/lib.rs
│   ├── ranker/             # Ranker trait, NoopRanker, GrpcRanker, WasmRanker
│   │   └── src/lib.rs
│   ├── router/             # scatter-gather, gRPC client, result merging, re-ranking
│   │   └── src/lib.rs
│   └── server/             # HTTP API (axum), CLI, main binary
│       └── src/main.rs
└── tests/                  # integration tests
    └── ...
```

| Crate | Depends On | Responsibility |
|-------|-----------|----------------|
| `core` | — | Types, schema, config, error types |
| `proto` | — | Generated gRPC stubs and protobuf types (tonic + prost) |
| `analysis` | `core` | Text analysis pipeline, tokenizers, stemmers per language |
| `index` | `core`, `analysis` | On-disk index structures: inverted index (FST + postings), column store, doc store, segment reader/writer |
| `query` | `core`, `index`, `analysis` | Query parsing, BM25 scoring, Levenshtein automaton, filter execution, aggregation |
| `shard` | `core`, `proto`, `index`, `query` | Shard lifecycle: WAL, write buffer, flush, segment merge, gRPC server |
| `ranker` | `core`, `proto` | Ranker trait + implementations: NoopRanker, GrpcRanker (tonic client), WasmRanker (wasmtime) |
| `router` | `core`, `proto`, `shard`, `ranker` | gRPC client to shards, scatter-gather fan-out, result merge, re-ranking, pagination |
| `server` | `core`, `router`, `shard` | HTTP layer (axum), CLI (`--mode router|shard|standalone`), main entry point |

## 10. Performance Considerations

### 9.1 Hot Path Optimizations

| Technique | Where | Why |
|-----------|-------|-----|
| Memory-mapped files | Inverted index, column store | Zero-copy reads, OS manages page cache |
| FST for term dictionary | Inverted index | Compact (~1/5 of hash map), supports fuzzy/prefix natively |
| PFOR-delta compression | Posting lists | Fast decode, cache-friendly sequential access |
| Dictionary encoding | Column store (keywords) | Aggregations reduce to u32 histograms |
| Roaring bitmaps | Deletion tracking, filter results | Compressed bitsets with fast AND/OR |
| Arena allocators | Per-query allocations | Avoid heap fragmentation on hot path |

### 9.2 Concurrency Model

- **Reads:** Fully concurrent across segments and shards. No locks on the read path — segments are immutable, write buffer is copy-on-swap
- **Writes:** Serialized per-shard through the WAL. Write buffer uses a double-buffer (active + standby) to avoid blocking readers during flush
- **Flush/Merge:** Background tokio tasks. Merge is CPU-heavy but does not block queries — new segment is swapped in atomically

### 9.3 Memory Budget

At 1B documents with ~16M per shard (64 shards):
- FST term dictionary: ~50-100MB per shard (compressed)
- Posting list hot pages: managed by OS page cache
- Column store hot pages: managed by OS page cache
- Write buffer: 64MB per shard
- Estimated working set per shard: ~200-300MB RAM

## 11. Security Considerations

- **Input validation:** All API inputs are validated (max query length, max filter count, max document size). Malformed requests return 400
- **Resource limits:** Per-query timeout (default 5s). Max result set size. Max concurrent queries
- **No auth in v1:** As scoped in requirements. The engine is expected to sit behind a gateway/proxy that handles auth
- **No arbitrary code execution:** Query language is declarative JSON, not a scripting language
- **Disk usage:** Configurable max index size per shard to prevent disk exhaustion

## 12. Deployment Architecture

### 12.1 Kubernetes Cluster Layout

```
                    ┌──────────────────────────────┐
                    │      K8s Ingress / LB         │
                    └──────────────┬────────────────┘
                                   │  HTTP
                    ┌──────────────▼────────────────┐
                    │   router-service (ClusterIP)   │
                    └──┬──────────────────────────┬─┘
                       │                          │
              ┌────────▼────────┐      ┌──────────▼──────┐
              │  Router Pod 0   │      │  Router Pod 1   │
              │  (Deployment)   │      │  (Deployment)   │
              │  replicas: 2+   │      │  replicas: 2+   │
              └───┬────┬────┬───┘      └──┬────┬────┬────┘
                  │    │    │             │    │    │
         gRPC     │    │    └──────┐ ┌────┘    │    │
                  │    │           │ │         │    │
    ┌─────────────▼─┐ ┌▼───────────▼─▼┐ ┌─────▼────▼─────┐
    │  shard-0 Pod  │ │  shard-1 Pod  │ │  shard-N Pod   │
    │(StatefulSet)  │ │(StatefulSet)  │ │(StatefulSet)   │
    │               │ │               │ │                │
    │ ┌───────────┐ │ │ ┌───────────┐ │ │ ┌────────────┐ │
    │ │ PVC (SSD) │ │ │ │ PVC (SSD) │ │ │ │ PVC (SSD)  │ │
    │ └───────────┘ │ │ └───────────┘ │ │ └────────────┘ │
    └───────────────┘ └───────────────┘ └────────────────┘
```

### 12.2 K8s Resource Summary

| Resource | Kind | Details |
|----------|------|---------|
| **Router** | `Deployment` | Stateless, horizontally scalable. 2+ replicas behind a `ClusterIP` Service |
| **Shard** | `StatefulSet` | Stateful, stable network identity (`shard-{i}.shards.{ns}.svc`). Each pod has a `PersistentVolumeClaim` for SSD-backed storage |
| **Router Service** | `Service` (ClusterIP) | Exposes HTTP API to ingress / internal clients |
| **Shard Service** | `Service` (Headless) | Enables DNS-based shard discovery for routers. No ClusterIP — routers connect to individual pods |
| **PVC** | `PersistentVolumeClaim` | One per shard pod, SSD storage class, sized per shard data volume |

### 12.3 Configuration

The binary is configured via environment variables and/or a config file:

```yaml
# search-engine.yaml
mode: router | shard | standalone

router:
  http_port: 8080
  shard_endpoints:             # explicit list OR discovered via K8s DNS
    - shard-0.shards:9001
    - shard-1.shards:9001
  shard_discovery: dns          # "static" or "dns"
  dns_service: shards.default.svc.cluster.local

shard:
  grpc_port: 9001
  shard_id: 0                  # set via StatefulSet ordinal (hostname)
  data_dir: /data
  num_shards: 16               # total shard count (for hash routing)
  write_buffer_size: 67108864  # 64MB
  merge_threads: 2
```

In K8s, `shard_id` is derived automatically from the StatefulSet pod name (`shard-0` → id 0).

### 12.4 Dockerfile

```dockerfile
# Build stage
FROM rust:1.78-slim AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --bin search-engine

# Runtime stage
FROM gcr.io/distroless/cc-debian12
COPY --from=builder /src/target/release/search-engine /
ENTRYPOINT ["/search-engine"]
```

### 12.5 Development Mode

For local development and testing, `--mode standalone` runs everything in a single process:

```
search-engine --mode standalone --num-shards 4 --data-dir ./data
```

This starts a router + 4 in-process shard servers, all communicating via gRPC on localhost.

## 13. Technical Risks and Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|-----------|------------|
| Fuzzy search latency at 1B scale | p99 > 200ms if Levenshtein automaton intersects too many terms | Medium | Limit max edit distance per query term. Precompute common fuzzy expansions. Short-circuit after N matches |
| Segment merge storms | Burst of merges blocks CPU for queries | Medium | Rate-limit merges. Separate merge thread pool from query pool. Tiered merge policy to reduce frequency |
| Memory-mapped I/O unpredictability | OS page faults on cold pages add latency spikes | Medium | Pre-warm hot segments on startup. Monitor page fault rate. Consider direct I/O for predictable paths |
| WAL grow unbounded on write-heavy loads | Disk fill if flush can't keep up | Low | Backpressure: slow down writes when WAL exceeds size threshold. Monitor WAL size |
| Stemmer false positives across languages | "casa" (house in ES/PT) stemmed incorrectly when language misdetected | Medium | Require explicit language per document/field. No auto-detection in v0 (P2: FR-26). Each field has one analyzer |
| Shard node failure | Partition becomes unavailable until pod restarts | Medium | K8s restarts crashed pods automatically. WAL ensures no data loss. Future: shard replication (FR-17) adds redundancy |
| Network partitions between router and shards | Partial results or query failures | Medium | Router implements timeouts + partial result mode (return results from available shards with degradation flag). Health checks detect down shards |
| gRPC overhead vs in-process | Serialization/deserialization adds latency | Low | Protobuf is fast (~microseconds). gRPC connection pooling via tonic. Measured overhead expected < 1ms per hop |
| StatefulSet rescheduling | Shard pod moves to new node, cold disk cache | Low | PVC follows the pod. Pre-warm FST on startup. Accept brief latency spike during reschedule |
| Re-ranker latency spikes | External ranker exceeds 30ms budget, degrading user experience | Medium | Hard 30ms timeout with BM25 fallback. Response includes `reranked` flag so client knows. Monitor fallback rate |
| WASM plugin safety | Buggy WASM module could panic or allocate unbounded memory | Low | wasmtime sandboxing limits memory/CPU. Module is validated at load time. Timeout applies to WASM too |

## 14. P2 Extension Points Summary

Every P2 feature has a designated integration point that v1's architecture preserves:

| P2 Feature | Extension Point |
|------------|----------------|
| FR-22: Synonyms | `TokenFilter` trait in analysis pipeline — add `SynonymFilter` |
| FR-23: Stop words | `TokenFilter` trait in analysis pipeline — add `StopWordFilter` |
| FR-24: Dynamic resharding | `ShardClient` trait + consistent hash ring already in place. Add shard migration protocol and data transfer gRPC stream |
| FR-25: Query caching | Insert cache layer in router before scatter-gather. Key on (query, filters, page) |
| FR-26: Language detection | Add `LanguageDetector` before analyzer selection. Route to per-language analyzer |
