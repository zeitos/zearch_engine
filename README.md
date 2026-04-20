# search-engine

A distributed full-text search engine written in Rust. Designed for large catalogues — fast BM25 retrieval, pluggeable re-ranking, horizontal sharding with replication, post-retrieval availability filtering by destination zone, and an independent autocomplete cluster.

## Features

- **BM25 full-text search** with AND retrieval and OR fallback
- **Typo tolerance** via FST Levenshtein automata
- **Multi-language stemming** — English, Spanish, Portuguese (configurable per field)
- **Filters** — equality, range, multi-value
- **Aggregations** — facet counts per field
- **Custom sort** — any numeric or string attribute, ASC/DESC
- **Dynamic attributes** — arbitrary key/value pairs on every document, usable for filtering and aggregations without schema changes
- **Configurable schema** — per-field control over indexing, storage, filtering, aggregation, boost, and text analysis pipeline
- **Horizontal sharding** — jump consistent hash routing, parallel fan-out and merge
- **Primary/replica replication** — async WAL forwarding, FullSync catch-up on restart
- **Pluggeable re-ranker** — BM25 fallback, gRPC ranker, or WASM module; automatic fallback on timeout
- **Pluggeable availability filtering** — post-retrieval zone-based stock filtering; three built-in strategies
- **Lean responses** — `include_docs: false` (default) returns only IDs + scores with no extra round-trip to shards
- **In-process query cache** — W-TinyLFU eviction, TTL, `destination_zone`-aware cache keys
- **Circuit breaker per shard** — skips open endpoints on read fan-out, fail-open
- **Autocomplete / suggest** — independent suggest cluster with its own SLA; eventual consistency, lossy by design
- **OpenTelemetry tracing** — OTLP export (Datadog, New Relic, Jaeger, Tempo); runtime-configurable
- **Prometheus metrics** — query latency, indexing throughput, shard doc counts, ranker, availability, cache, and circuit breaker stats
- **Kubernetes-native** — StatefulSets with PVCs, headless services, ServiceMonitor, PrometheusRule

## Architecture

```
                    ┌─────────────────┐
  HTTP clients ────►│     Router      │
                    │  (stateless)    │
                    └────────┬────────┘
                             │
           ┌─────────────────┼──────────────────────┐
           │ search fan-out  │                       │ suggest fan-out
           │ (gRPC)          │                       │ (fire-and-forget)
           ▼                 ▼                       ▼
   ┌─────────────┐   ┌─────────────┐        ┌──────────────┐
   │  Shard 0    │   │  Shard N    │  ...   │ Suggest 0    │
   │  (primary)  │   │  (primary)  │        │ (no WAL)     │
   └──────┬──────┘   └──────┬──────┘        └──────────────┘
          │ async WAL        │
   ┌──────▼──────┐          ...
   │  Replica 0  │
   └─────────────┘
```

### Search cluster

The router fans out every search to all shards in parallel, merges the scored hits, optionally re-ranks, applies availability filtering, and paginates. Shards return only `(doc_id, score)` pairs — no document payload travels the network on the search path unless `include_docs: true` is requested.

Writes go to the primary only. Replicas receive writes asynchronously via a WAL queue and catch up on restart automatically — WAL replay if the primary still has the relevant entries, FullSync (streaming all live segments) otherwise.

Both search and suggest clusters are **eventually consistent**. The distinction is durability:

| Cluster | Durability | On crash |
|---------|-----------|----------|
| Search (primary) | WAL — survives crashes | Replays WAL on restart |
| Search (replica) | Async replication | FullSync from primary |
| Suggest | None — lossy by design | Loses recent writes |

### Suggest cluster

Suggest shards run the same binary in `mode: suggest`. They receive index/bulk writes from the router fire-and-forget (short timeout, errors swallowed). No WAL, no replication, no replicas in v1. This allows independent scaling, deployment, and SLA from the search cluster.

The long-term feeding model is two independent Kafka consumers — one for search, one for suggest — replacing the router fan-out entirely. No changes to the shards are needed for that migration.

## Document model

Each document has a set of **base fields** plus an open-ended **attributes map**:

```json
{
  "id": 42,
  "title": "Short descriptive name",
  "description": "Longer free-text content",
  "price": 299.99,
  "category": "some-category",
  "attributes": {
    "brand": "Acme",
    "color": "red",
    "stock_buenos_aires": 15,
    "stock_santa_cruz": 0,
    "tags": ["sale", "new-arrival"]
  }
}
```

`attributes` values can be strings, numbers, booleans, or string arrays. Any attribute can be used for filtering, aggregation, sorting, or availability filtering without modifying the engine.

### Schema configuration

The index schema controls, per field, whether it is:

- **indexed** — included in the inverted index for full-text search
- **stored** — retrievable when `include_docs: true`
- **filterable** — available as a filter dimension
- **aggregatable** — available for facet counts
- **boost** — relevance weight multiplier (default 1.0)
- **analyzer** — text analysis pipeline: `standard` (tokenize + stem) or `keyword` (exact match)

The default schema indexes `title` (boost 2.0) and `description` (boost 1.0) with stemming, and makes `price` and `category` filterable/aggregatable. Custom schemas are defined in code via `IndexSchema`.

## Crate layout

| Crate | Role |
|---|---|
| `search-core` | Shared types: `Document`, `SearchRequest`, `SearchResponse`, `IndexSchema`, config |
| `search-analysis` | Unicode tokenizer, lowercase filter, multi-language stemmer |
| `search-index` | Segment writer/reader: inverted index, FST term dictionary, roaring bitmaps |
| `search-query` | BM25 scorer, multi-segment searcher, aggregations |
| `search-shard` | Shard engine: write buffer, WAL, segment merging, replication manager |
| `search-router` | Fan-out router, shard groups, availability strategies, re-ranker wiring |
| `search-ranker` | Re-ranker trait: Noop, gRPC, WASM |
| `search-proto` | Protobuf definitions (tonic) |
| `search-server` | Binary: HTTP (axum) + gRPC (tonic) server |

## Quick start

### Standalone (single process)

```bash
cargo build --release
./target/release/search-server --mode standalone --num-shards 4 --http-port 8080
```

### Index a document

```bash
curl -X POST http://localhost:8080/v1/index \
  -H 'Content-Type: application/json' \
  -d '{
    "id": 1,
    "title": "Wireless noise-cancelling headphones",
    "description": "Over-ear, 30h battery, foldable",
    "price": 199.99,
    "category": "audio",
    "attributes": {
      "brand": "Sony",
      "color": "black",
      "stock_zone_a": 12,
      "stock_zone_b": 0
    }
  }'
```

### Bulk index

```bash
curl -X POST http://localhost:8080/v1/bulk \
  -H 'Content-Type: application/json' \
  -d '[
    {"id": 1, "title": "Wireless headphones", "price": 199.99, "category": "audio",       "attributes": {"brand": "Sony"}},
    {"id": 2, "title": "Mechanical keyboard", "price": 129.99, "category": "peripherals", "attributes": {"brand": "Keychron"}},
    {"id": 3, "title": "4K monitor 27 inch",  "price": 449.99, "category": "monitors",    "attributes": {"brand": "LG"}}
  ]'
```

### Search

**Default — IDs and scores only**

```bash
curl -X POST http://localhost:8080/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "wireless headphones"}'
```

```json
{
  "hits": [
    {"id": 1, "score": 3.42},
    {"id": 7, "score": 1.85}
  ],
  "total_hits": 2,
  "aggregations": {},
  "reranked": false,
  "took_ms": 3
}
```

**With full documents**

```bash
curl -X POST http://localhost:8080/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "wireless headphones", "include_docs": true}'
```

**With filters, aggregations, and sort**

```bash
curl -X POST http://localhost:8080/v1/search \
  -H 'Content-Type: application/json' \
  -d '{
    "query": "headphones",
    "filters": {
      "category": {"eq": "audio"},
      "price":    {"gte": 50, "lte": 500},
      "brand":    {"in": ["Sony", "Bose", "Sennheiser"]}
    },
    "aggregations": ["category", "brand"],
    "sort": {"field": "price", "order": "asc"},
    "offset": 0,
    "limit": 20
  }'
```

**With availability filtering by zone**

```bash
curl -X POST http://localhost:8080/v1/search \
  -H 'Content-Type: application/json' \
  -d '{
    "query": "headphones",
    "destination_zone": "zone_a",
    "limit": 20
  }'
```

Items where `attributes["stock_zone_a"]` is 0 or absent (when `default_available: false`) are removed before pagination. The scatter phase fetches 3× more candidates to keep the result page full after filtering.

**With typo tolerance disabled**

```bash
curl -X POST http://localhost:8080/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "headphoens", "typo_tolerance": false}'
```

### Bulk ingest (catalogue format)

Accepts a catalogue-format JSON array or NDJSON stream and maps it to the internal document format automatically. Useful for importing from external catalogue sources without pre-processing.

```bash
# JSON array
curl -X POST http://localhost:8080/v1/ingest/bulk \
  -H 'Content-Type: application/json' \
  --data-binary @catalogue.json

# NDJSON — one item per line, streaming-friendly
curl -X POST http://localhost:8080/v1/ingest/bulk \
  -H 'Content-Type: application/x-ndjson' \
  --data-binary @catalogue.ndjson
```

### Autocomplete / suggest

```bash
curl "http://localhost:8080/v1/suggest?q=wire&limit=5"
```

```json
{
  "suggestions": [
    { "term": "wireless", "score": 0.94 },
    { "term": "wired",    "score": 0.61 },
    { "term": "wire",     "score": 0.12 }
  ],
  "consistency": "eventual",
  "took_ms": 3
}
```

Suggestions are ranked by document frequency. The `consistency: "eventual"` field is
explicit — suggest shards have no WAL and may lag behind the search index. See
[Consistency model](#consistency-model) below.

Optional parameters: `field` (default `title`), `limit` (1–20, default 5).

### Delete

```bash
curl -X DELETE http://localhost:8080/v1/index/1
```

### Stats

```bash
curl http://localhost:8080/v1/stats
```

```json
{
  "total_docs": 250000,
  "total_segments": 8,
  "shards": [
    {
      "shard_id": 0,
      "doc_count": 62500,
      "segment_count": 2,
      "replicas": [{"shard_id": 0, "doc_count": 62500, "segment_count": 2}]
    }
  ]
}
```

### Admin

```bash
# Force flush write buffer to segment
curl -X POST http://localhost:8080/v1/admin/flush

# Rebuild index from stored documents (e.g. after schema change)
curl -X POST http://localhost:8080/v1/admin/reindex
```

## Configuration

### Router mode

```yaml
mode: router
router:
  http_port: 8080
  query_timeout_ms: 5000
  shards:
    - primary: "http://shard-0:9001"
      replicas:
        - "http://shard-replica-0:9001"
    - primary: "http://shard-1:9001"
      replicas:
        - "http://shard-replica-1:9001"
  # Optional suggest cluster (independent SLA, eventual consistency)
  suggest_shards:
    - "http://suggest-0:9002"
    - "http://suggest-1:9002"
  suggest_write_timeout_ms: 200   # fire-and-forget; errors swallowed
  suggest_query_timeout_ms: 100
  availability:
    strategy: static          # noop | static | region_stock
    static:
      stock_attribute_prefix: "stock_"
      default_available: true
      min_stock: 1
ranker:
  type: none                  # none | grpc | wasm
  timeout_ms: 30
  candidates: 200
```

### Shard mode

```yaml
mode: shard
shard:
  grpc_port: 9001
  shard_id: 0
  num_shards: 4
  data_dir: /data
  write_buffer_size: 67108864   # 64 MB
  merge_threads: 2
  role: primary                 # primary | replica
  replica_endpoints:
    - "http://shard-replica-0:9001"
```

### Suggest mode

```yaml
mode: suggest
suggest_shard:
  grpc_port: 9002
  num_suggest_shards: 2
  min_doc_frequency: 2      # terms in fewer docs are excluded
  max_terms_per_shard: 50   # max candidates returned per suggest request
```

Suggest shards have no WAL. On restart they start with an empty index and repopulate
as writes arrive. This is intentional — see [Consistency model](#consistency-model).

### Availability strategies

| Strategy | Description | External calls |
|---|---|---|
| `noop` | No filtering — all candidates pass (default) | None |
| `static` | Reads `attributes["stock_{zone}"]` from the indexed document | None |
| `region_stock` | `GET {base_url}/availability?zone=X&doc_ids=1,2,3` → `{"available_ids":[...]}` | Yes (fail-open on timeout) |

```yaml
# region_stock config
availability:
  strategy: region_stock
  region_stock:
    base_url: "http://stock-service:8080"
    timeout_ms: 50
```

## Kubernetes deployment

```bash
kubectl apply -f k8s/namespace.yaml
kubectl apply -f k8s/configmap.yaml
kubectl apply -f k8s/shard-service.yaml
kubectl apply -f k8s/shard-statefulset.yaml
kubectl apply -f k8s/shard-replica-service.yaml
kubectl apply -f k8s/shard-replica-statefulset.yaml
kubectl apply -f k8s/router-service.yaml
kubectl apply -f k8s/router-deployment.yaml
```

Shard pods auto-detect their `shard_id` from the StatefulSet hostname (`shard-2` → `shard_id=2`). Primary and replica endpoints are resolved from templates at startup — a single ConfigMap entry covers all pods in a StatefulSet.

**Pause and resume without data loss:**

```bash
# Pause
kubectl scale statefulset/shard         --replicas=0 -n search
kubectl scale statefulset/shard-replica --replicas=0 -n search
kubectl scale deployment/router         --replicas=0 -n search

# Resume
kubectl scale statefulset/shard         --replicas=4 -n search
kubectl scale statefulset/shard-replica --replicas=4 -n search
kubectl scale deployment/router         --replicas=2 -n search
```

PVCs are retained when StatefulSets are scaled to 0. Replicas catch up automatically on restart.

## Consistency model

Both clusters are eventually consistent. The difference is durability:

| Cluster | Durability | Consistency | On crash |
|---------|-----------|-------------|----------|
| Search primary | WAL — every write is persisted before ack | Eventual + durable | Replays WAL; no data loss |
| Search replica | Async replication from primary | Eventual | FullSync from primary on restart |
| Suggest | None — no WAL | Eventual + lossy | Loses writes since last flush |

Suggest shards are **intentionally lossy**. A crashed or restarted suggest shard
repopulates from subsequent write traffic — no special recovery procedure needed.
The `"consistency": "eventual"` field in suggest responses documents this contract.

**Future feeding model (Kafka):** replace the router fan-out with two independent
consumers. No changes to the shard code:

```
Kafka: catalogue.items
    ├──► Consumer A → search shards   (durable path)
    └──► Consumer B → suggest shards  (lossy path)
```

## Query cache

An optional in-process LRU cache at the router. Cache hits skip the scatter/merge/rerank/availability pipeline entirely.

```yaml
router:
  cache:
    enabled: true
    max_entries: 10000   # W-TinyLFU eviction
    ttl_seconds: 60
```

- Disabled by default (`enabled: false`)
- Cache key covers all query parameters except `include_docs` — the same entry serves both `true` and `false`
- `destination_zone` is part of the key — different zones get independent entries
- `cache_hit: bool` in the response indicates whether the result was served from cache
- Per-router-instance; no cross-replica sharing

## Re-ranker

The re-ranker runs on a bounded candidate pool after the scatter/merge phase, so its cost is independent of index size.

| Type | Description |
|---|---|
| `none` | Keeps BM25 order (default) |
| `grpc` | Calls an external ranking service; automatic fallback to BM25 on timeout |
| `wasm` | Loads a `.wasm` module via wasmtime; sandboxed, no system access |

Fallback behaviour: if the ranker exceeds `timeout_ms`, the query completes immediately using BM25 scores. The `reranked` field in the response indicates whether re-ranking was applied.

## Observability

### Prometheus

Metrics at `GET /metrics`. Use `k8s/servicemonitor.yaml` for automatic scraping via
Prometheus Operator, and `k8s/prometheusrule.yaml` for pre-built alerting rules
(high latency, circuit breaker open, ranker fallback rate, cache hit rate).

| Metric | Type | Description |
|---|---|---|
| `search_queries_total` | counter | Total search queries |
| `search_query_duration_seconds` | histogram | End-to-end query latency |
| `index_docs_total` | counter | Total documents indexed |
| `ranker_requests_total` | counter | Re-ranker invocations |
| `ranker_fallbacks_total` | counter | Re-ranker timeouts / fallbacks to BM25 |
| `ranker_rerank_total` | counter | Successful re-ranks |
| `shard_doc_count` | gauge | Documents per shard (label: `shard_id`) |
| `shard_segment_count` | gauge | Segments per shard |
| `availability_filter_removed_total` | counter | Candidates removed by availability strategy |
| `availability_filter_duration_seconds` | histogram | Time spent in availability filter |
| `availability_stock_service_errors_total` | counter | Stock service timeouts / errors |
| `search_cache_hits_total` | counter | Query cache hits |
| `search_cache_misses_total` | counter | Query cache misses |
| `search_cache_size` | gauge | Current number of cached entries |
| `shard_circuit_open_total` | counter | Circuit breaker open events |
| `suggest_requests_total` | counter | Total suggest requests |
| `suggest_duration_seconds` | histogram | End-to-end suggest latency |
| `suggest_write_errors_total` | counter | Dropped fire-and-forget suggest writes |

### OpenTelemetry (distributed tracing)

Traces are exported via OTLP gRPC — compatible with Datadog, New Relic, Grafana
Tempo, Jaeger, and any OpenTelemetry Collector. Disabled by default; enabled per
config with no recompilation needed.

```yaml
telemetry:
  enabled: true
  otlp_endpoint: "http://otel-collector:4317"
  service_name: "search-router"
  sample_rate: 0.1   # sample 10% in high-traffic environments
```

Instrumented spans: HTTP requests (method, path, status), `router.search` (query,
total_hits, cache_hit, healthy_shards, reranked). On graceful shutdown, in-flight
spans are flushed before the process exits.

## Building

```bash
cargo build --release
cargo test
cargo test -- --ignored   # slow benchmarks (100k docs)
```

Requires Rust 1.75+. No system dependencies — TLS via rustls, no OpenSSL required.

## Capacity planning

Approximate index size: **~5 KB per document** (raw storage + inverted index + FST term dictionary).

| Documents | Recommended shards | RAM per shard | Storage per shard |
|---|---|---|---|
| 10M | 2 | 8 GB | 25 GB |
| 100M | 8 | 32 GB | 65 GB |
| 1B | 32 | 64 GB | 160 GB |

The re-ranker and availability filter operate on a bounded candidate pool after retrieval, so their overhead is constant regardless of index size.
