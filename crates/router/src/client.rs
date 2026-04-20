use search_core::{Document, SearchRequest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Per-shard search result (scored doc IDs only — no full docs yet).
#[derive(Debug)]
pub struct ShardSearchResult {
    pub hits: Vec<(u64, f32)>, // (global_doc_id, score)
    pub total_hits: u64,
    pub aggregations: HashMap<String, Vec<(String, u64)>>,
    pub used_or_fallback: bool,
}

/// Abstract interface to a shard — implemented by both local and remote clients.
#[async_trait::async_trait]
pub trait ShardClient: Send + Sync {
    /// Returns true when a circuit breaker has opened for this endpoint.
    /// Default implementation always returns false (no circuit breaker).
    fn is_circuit_open(&self) -> bool { false }

    async fn search(&self, request: SearchRequest) -> search_core::Result<ShardSearchResult>;
    async fn index(&self, doc: Document) -> search_core::Result<()>;
    async fn bulk(&self, docs: Vec<Document>) -> search_core::Result<u32>;
    async fn delete(&self, doc_id: u64) -> search_core::Result<()>;
    async fn get_docs(&self, doc_ids: &[u64]) -> search_core::Result<Vec<Document>>;
    async fn stats(&self) -> search_core::Result<search_shard::ShardStats>;
    async fn flush(&self) -> search_core::Result<()>;
    async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats>;
    async fn health(&self) -> bool;

    /// Prefix suggestion. Default: no-op for search shards.
    /// Overridden by suggest shard clients.
    async fn suggest(
        &self,
        _prefix: &str,
        _field: &str,
        _limit: usize,
    ) -> search_core::Result<Vec<search_suggest::SuggestTerm>> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// ShardGroup — primary + replicas for one logical shard
// ---------------------------------------------------------------------------

/// Groups a primary and its replicas. Writes always go to the primary;
/// reads are distributed round-robin across all healthy endpoints.
pub struct ShardGroup {
    primary: Arc<dyn ShardClient>,
    replicas: Vec<Arc<dyn ShardClient>>,
    next: AtomicUsize,
}

impl ShardGroup {
    pub fn new(primary: Arc<dyn ShardClient>, replicas: Vec<Arc<dyn ShardClient>>) -> Self {
        Self { primary, replicas, next: AtomicUsize::new(0) }
    }

    pub fn primary_only(primary: Arc<dyn ShardClient>) -> Self {
        Self::new(primary, vec![])
    }

    /// Always returns the primary for writes.
    pub fn write_target(&self) -> &Arc<dyn ShardClient> {
        &self.primary
    }

    /// Round-robin across primary + all replicas for reads, skipping open circuits.
    /// Falls back to primary if all endpoints have open circuits (fail-open).
    pub fn read_target(&self) -> &Arc<dyn ShardClient> {
        let total = 1 + self.replicas.len();
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        for i in 0..total {
            let idx = (start + i) % total;
            let client = if idx == 0 { &self.primary } else { &self.replicas[idx - 1] };
            if !client.is_circuit_open() {
                return client;
            }
        }
        &self.primary
    }

    pub fn replica_count(&self) -> usize {
        self.replicas.len()
    }

    pub fn replica_clients(&self) -> &[Arc<dyn ShardClient>] {
        &self.replicas
    }
}

// ---------------------------------------------------------------------------
// LocalShardClient — direct in-process call, no gRPC overhead
// ---------------------------------------------------------------------------

pub struct LocalShardClient {
    pub shard: Arc<search_shard::ShardEngine>,
}

impl LocalShardClient {
    pub fn new(shard: Arc<search_shard::ShardEngine>) -> Self {
        Self { shard }
    }
}

#[async_trait::async_trait]
impl ShardClient for LocalShardClient {
    async fn search(&self, request: SearchRequest) -> search_core::Result<ShardSearchResult> {
        let resp = self.shard.search(request)?;
        let hits = resp.hits.iter().map(|h| (h.id, h.score)).collect();
        let aggregations = resp
            .aggregations
            .into_iter()
            .map(|(field, buckets)| {
                (field, buckets.into_iter().map(|b| (b.value, b.count)).collect())
            })
            .collect();
        Ok(ShardSearchResult {
            hits,
            total_hits: resp.total_hits,
            aggregations,
            used_or_fallback: resp.retrieval_mode == search_core::RetrievalMode::OrFallback,
        })
    }

    async fn index(&self, doc: Document) -> search_core::Result<()> {
        self.shard.index(doc)
    }

    async fn bulk(&self, docs: Vec<Document>) -> search_core::Result<u32> {
        let count = docs.len() as u32;
        self.shard.index_batch(docs)?;
        Ok(count)
    }

    async fn delete(&self, doc_id: u64) -> search_core::Result<()> {
        self.shard.delete(doc_id)
    }

    async fn get_docs(&self, doc_ids: &[u64]) -> search_core::Result<Vec<Document>> {
        self.shard.get_docs(doc_ids)
    }

    async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
        Ok(self.shard.stats())
    }

    async fn flush(&self) -> search_core::Result<()> {
        self.shard.flush()
    }

    async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats> {
        self.shard.reindex_self()
    }

    async fn health(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// LocalSuggestClient — wraps SuggestShardEngine for standalone mode
// ---------------------------------------------------------------------------

pub struct LocalSuggestClient {
    engine: Arc<search_suggest::SuggestShardEngine>,
}

impl LocalSuggestClient {
    pub fn new(engine: Arc<search_suggest::SuggestShardEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait::async_trait]
impl ShardClient for LocalSuggestClient {
    async fn search(&self, _request: SearchRequest) -> search_core::Result<ShardSearchResult> {
        Err(search_core::Error::Config(
            "suggest shard does not implement search".into(),
        ))
    }

    async fn index(&self, doc: Document) -> search_core::Result<()> {
        self.engine.index(&doc);
        Ok(())
    }

    async fn bulk(&self, docs: Vec<Document>) -> search_core::Result<u32> {
        let n = docs.len() as u32;
        self.engine.bulk(&docs);
        Ok(n)
    }

    async fn delete(&self, _doc_id: u64) -> search_core::Result<()> {
        Ok(())
    }

    async fn get_docs(&self, _doc_ids: &[u64]) -> search_core::Result<Vec<Document>> {
        Ok(vec![])
    }

    async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
        Ok(search_shard::ShardStats {
            doc_count: self.engine.term_count() as u64,
            segment_count: 0,
            buffer_doc_count: 0,
            buffer_size_bytes: 0,
            shard_id: self.engine.config().shard_id,
        })
    }

    async fn flush(&self) -> search_core::Result<()> {
        self.engine.flush();
        Ok(())
    }

    async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats> {
        Ok(search_shard::ReindexStats {
            shard_id: self.engine.config().shard_id,
            docs_reindexed: 0,
            elapsed_ms: 0,
        })
    }

    async fn health(&self) -> bool {
        true
    }

    async fn suggest(
        &self,
        prefix: &str,
        field: &str,
        limit: usize,
    ) -> search_core::Result<Vec<search_suggest::SuggestTerm>> {
        Ok(self.engine.suggest(prefix, field, limit))
    }
}

// ---------------------------------------------------------------------------
// RemoteShardClient — connects to a shard via gRPC
// ---------------------------------------------------------------------------

use search_proto::shard::shard_service_client::ShardServiceClient;
use search_proto::shard::{
    filter_value, AggregationResult, BulkIndexRequest, DeleteRequest, DocumentProto,
    FilterValue as ProtoFilterValue, FlushRequest, GetDocsRequest, HealthRequest, IndexRequest,
    MultiValueFilter, RangeFilter, ReindexRequest, SearchRequest as ProtoSearchRequest,
    SortOrder as ProtoSortOrder, SortSpec as ProtoSortSpec, StatsRequest, SuggestRequest,
};
use search_core::{FilterValue, SortOrder, Value};
use tonic::transport::Channel;

pub struct RemoteShardClient {
    endpoint: String,
}

impl RemoteShardClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self { endpoint: endpoint.into() }
    }

    async fn connect(&self) -> search_core::Result<ShardServiceClient<Channel>> {
        ShardServiceClient::connect(self.endpoint.clone())
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))
    }
}

fn doc_to_proto(doc: Document) -> DocumentProto {
    DocumentProto {
        id: doc.id,
        title: doc.title,
        description: doc.description,
        price: doc.price,
        category: doc.category,
        attributes: doc
            .attributes
            .into_iter()
            .filter_map(|(k, v)| {
                if let Value::String(s) = v { Some((k, s)) } else { None }
            })
            .collect(),
    }
}

fn proto_to_doc(proto: DocumentProto) -> Document {
    Document {
        id: proto.id,
        title: proto.title,
        description: proto.description,
        price: proto.price,
        category: proto.category,
        attributes: proto
            .attributes
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    }
}

fn search_request_to_proto(req: SearchRequest) -> ProtoSearchRequest {
    let filters = req
        .filters
        .into_iter()
        .map(|(field, fv)| {
            let proto_filter = match fv {
                FilterValue::Equality { eq } => ProtoFilterValue {
                    filter: Some(filter_value::Filter::Eq(eq)),
                },
                FilterValue::Range { gte, lte } => ProtoFilterValue {
                    filter: Some(filter_value::Filter::Range(RangeFilter { gte, lte })),
                },
                FilterValue::MultiValue { r#in } => ProtoFilterValue {
                    filter: Some(filter_value::Filter::Multi(MultiValueFilter { values: r#in })),
                },
            };
            (field, proto_filter)
        })
        .collect();

    let sort = req.sort.map(|s| ProtoSortSpec {
        field: s.field,
        order: match s.order {
            SortOrder::Asc => ProtoSortOrder::Asc as i32,
            SortOrder::Desc => ProtoSortOrder::Desc as i32,
        },
    });

    ProtoSearchRequest {
        query: req.query,
        filters,
        aggregations: req.aggregations,
        sort,
        offset: req.offset as u64,
        limit: req.limit as u64,
        typo_tolerance: req.typo_tolerance,
        language: req.language,
    }
}

fn agg_result_to_counts(agg: AggregationResult) -> Vec<(String, u64)> {
    agg.buckets.into_iter().map(|b| (b.value, b.count)).collect()
}

#[async_trait::async_trait]
impl ShardClient for RemoteShardClient {
    async fn search(&self, request: SearchRequest) -> search_core::Result<ShardSearchResult> {
        let mut client = self.connect().await?;
        let proto_req = search_request_to_proto(request);
        let resp = client
            .search(tonic::Request::new(proto_req))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();

        let hits = resp.hits.into_iter().map(|h| (h.doc_id, h.score)).collect();
        let aggregations = resp
            .aggregations
            .into_iter()
            .map(|(field, agg)| (field, agg_result_to_counts(agg)))
            .collect();

        Ok(ShardSearchResult {
            hits,
            total_hits: resp.total_hits,
            aggregations,
            used_or_fallback: resp.retrieval_mode == "or_fallback",
        })
    }

    async fn index(&self, doc: Document) -> search_core::Result<()> {
        let mut client = self.connect().await?;
        client
            .index(tonic::Request::new(IndexRequest { document: Some(doc_to_proto(doc)) }))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?;
        Ok(())
    }

    async fn bulk(&self, docs: Vec<Document>) -> search_core::Result<u32> {
        let mut client = self.connect().await?;
        let documents: Vec<DocumentProto> = docs.into_iter().map(doc_to_proto).collect();
        let resp = client
            .bulk_index(tonic::Request::new(BulkIndexRequest { documents }))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();
        Ok(resp.indexed)
    }

    async fn delete(&self, doc_id: u64) -> search_core::Result<()> {
        let mut client = self.connect().await?;
        client
            .delete(tonic::Request::new(DeleteRequest { doc_id }))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?;
        Ok(())
    }

    async fn get_docs(&self, doc_ids: &[u64]) -> search_core::Result<Vec<Document>> {
        let mut client = self.connect().await?;
        let resp = client
            .get_docs(tonic::Request::new(GetDocsRequest { doc_ids: doc_ids.to_vec() }))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();
        Ok(resp.documents.into_iter().map(proto_to_doc).collect())
    }

    async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
        let mut client = self.connect().await?;
        let resp = client
            .stats(tonic::Request::new(StatsRequest {}))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();
        Ok(search_shard::ShardStats {
            doc_count: resp.doc_count,
            segment_count: resp.segment_count,
            buffer_doc_count: 0,
            buffer_size_bytes: 0,
            shard_id: resp.shard_id,
        })
    }

    async fn flush(&self) -> search_core::Result<()> {
        let mut client = self.connect().await?;
        client
            .flush(tonic::Request::new(FlushRequest {}))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?;
        Ok(())
    }

    async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats> {
        let mut client = self.connect().await?;
        let resp = client
            .reindex(tonic::Request::new(ReindexRequest {}))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();
        Ok(search_shard::ReindexStats {
            shard_id: resp.shard_id,
            docs_reindexed: resp.docs_reindexed,
            elapsed_ms: resp.elapsed_ms,
        })
    }

    async fn health(&self) -> bool {
        let Ok(mut client) = self.connect().await else { return false };
        client.health(tonic::Request::new(HealthRequest {})).await.is_ok()
    }

    async fn suggest(
        &self,
        prefix: &str,
        field: &str,
        limit: usize,
    ) -> search_core::Result<Vec<search_suggest::SuggestTerm>> {
        let mut client = self.connect().await?;
        let resp = client
            .suggest(tonic::Request::new(SuggestRequest {
                prefix: prefix.to_string(),
                field: field.to_string(),
                limit: limit as u32,
            }))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();
        Ok(resp
            .entries
            .into_iter()
            .map(|e| search_suggest::SuggestTerm { term: e.term, score: e.score })
            .collect())
    }
}
