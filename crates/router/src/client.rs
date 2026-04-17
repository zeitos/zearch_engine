use search_core::{Document, SearchRequest};
use std::collections::HashMap;
use std::sync::Arc;

/// Per-shard search result (scored doc IDs only — no full docs yet).
#[derive(Debug)]
pub struct ShardSearchResult {
    pub hits: Vec<(u64, f32)>, // (global_doc_id, score)
    pub total_hits: u64,
    pub aggregations: HashMap<String, Vec<(String, u64)>>,
}

/// Abstract interface to a shard — implemented by both local and remote clients.
#[async_trait::async_trait]
pub trait ShardClient: Send + Sync {
    async fn search(&self, request: SearchRequest) -> search_core::Result<ShardSearchResult>;
    async fn index(&self, doc: Document) -> search_core::Result<()>;
    async fn bulk(&self, docs: Vec<Document>) -> search_core::Result<u32>;
    async fn delete(&self, doc_id: u64) -> search_core::Result<()>;
    async fn get_docs(&self, doc_ids: &[u64]) -> search_core::Result<Vec<Document>>;
    async fn stats(&self) -> search_core::Result<search_shard::ShardStats>;
    async fn health(&self) -> bool;
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
        Ok(ShardSearchResult { hits, total_hits: resp.total_hits, aggregations })
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

    async fn health(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// RemoteShardClient — connects to a shard via gRPC
// ---------------------------------------------------------------------------

use search_proto::shard::shard_service_client::ShardServiceClient;
use search_proto::shard::{
    filter_value, AggregationResult, BulkIndexRequest, DeleteRequest, DocumentProto,
    FilterValue as ProtoFilterValue, GetDocsRequest, HealthRequest, IndexRequest, MultiValueFilter,
    RangeFilter, SearchRequest as ProtoSearchRequest, SortOrder as ProtoSortOrder,
    SortSpec as ProtoSortSpec, StatsRequest,
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

        Ok(ShardSearchResult { hits, total_hits: resp.total_hits, aggregations })
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

    async fn health(&self) -> bool {
        let Ok(mut client) = self.connect().await else { return false };
        client.health(tonic::Request::new(HealthRequest {})).await.is_ok()
    }
}
