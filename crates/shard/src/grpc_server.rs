use crate::shard::ShardEngine;
use search_core::{Document, FilterValue, SearchRequest, SortOrder, SortSpec, Value};
use search_proto::shard::shard_service_server::ShardService;
use search_proto::shard::{
    filter_value, AggregationBucket, AggregationResult, BulkIndexRequest, BulkIndexResponse,
    DeleteRequest, DeleteResponse, DocumentProto, GetDocsRequest, GetDocsResponse, HealthRequest,
    HealthResponse, IndexRequest, IndexResponse, ScoredDocument,
    SearchRequest as ProtoSearchRequest, SearchResponse as ProtoSearchResponse,
    SortOrder as ProtoSortOrder, StatsRequest, StatsResponse,
};
use std::collections::HashMap;
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub struct ShardGrpcServer {
    shard: Arc<ShardEngine>,
}

impl ShardGrpcServer {
    pub fn new(shard: Arc<ShardEngine>) -> Self {
        Self { shard }
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

fn proto_to_search_request(proto: ProtoSearchRequest) -> SearchRequest {
    let filters = proto
        .filters
        .into_iter()
        .filter_map(|(field, fv)| {
            let core_filter = match fv.filter? {
                filter_value::Filter::Eq(v) => FilterValue::Equality { eq: v },
                filter_value::Filter::Range(r) => FilterValue::Range { gte: r.gte, lte: r.lte },
                filter_value::Filter::Multi(m) => FilterValue::MultiValue { r#in: m.values },
            };
            Some((field, core_filter))
        })
        .collect();

    let sort = proto.sort.map(|s| SortSpec {
        field: s.field,
        order: if s.order == ProtoSortOrder::Asc as i32 {
            SortOrder::Asc
        } else {
            SortOrder::Desc
        },
    });

    SearchRequest {
        query: proto.query,
        filters,
        aggregations: proto.aggregations,
        sort,
        offset: proto.offset as usize,
        limit: proto.limit as usize,
        typo_tolerance: proto.typo_tolerance,
        language: proto.language,
    }
}

#[tonic::async_trait]
impl ShardService for ShardGrpcServer {
    async fn search(
        &self,
        request: Request<ProtoSearchRequest>,
    ) -> Result<Response<ProtoSearchResponse>, Status> {
        let req = proto_to_search_request(request.into_inner());

        let resp = self
            .shard
            .search(req)
            .map_err(|e| Status::internal(e.to_string()))?;

        let hits: Vec<ScoredDocument> = resp
            .hits
            .into_iter()
            .map(|h| ScoredDocument { doc_id: h.id, score: h.score })
            .collect();

        let aggregations: HashMap<String, AggregationResult> = resp
            .aggregations
            .into_iter()
            .map(|(field, buckets)| {
                let proto_buckets = buckets
                    .into_iter()
                    .map(|b| AggregationBucket { value: b.value, count: b.count })
                    .collect();
                (field, AggregationResult { buckets: proto_buckets })
            })
            .collect();

        Ok(Response::new(ProtoSearchResponse {
            hits,
            total_hits: resp.total_hits,
            aggregations,
        }))
    }

    async fn get_docs(
        &self,
        request: Request<GetDocsRequest>,
    ) -> Result<Response<GetDocsResponse>, Status> {
        let doc_ids = request.into_inner().doc_ids;
        let docs = self
            .shard
            .get_docs(&doc_ids)
            .map_err(|e| Status::internal(e.to_string()))?;

        let documents = docs.into_iter().map(doc_to_proto).collect();
        Ok(Response::new(GetDocsResponse { documents }))
    }

    async fn index(
        &self,
        request: Request<IndexRequest>,
    ) -> Result<Response<IndexResponse>, Status> {
        let proto_doc = request
            .into_inner()
            .document
            .ok_or_else(|| Status::invalid_argument("missing document"))?;
        let doc = proto_to_doc(proto_doc);
        self.shard
            .index(doc)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(IndexResponse { success: true }))
    }

    async fn bulk_index(
        &self,
        request: Request<BulkIndexRequest>,
    ) -> Result<Response<BulkIndexResponse>, Status> {
        let docs: Vec<Document> = request.into_inner().documents.into_iter().map(proto_to_doc).collect();
        let count = docs.len() as u32;
        self.shard
            .index_batch(docs)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(BulkIndexResponse { indexed: count }))
    }

    async fn delete(
        &self,
        request: Request<DeleteRequest>,
    ) -> Result<Response<DeleteResponse>, Status> {
        let doc_id = request.into_inner().doc_id;
        self.shard
            .delete(doc_id)
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(DeleteResponse { success: true }))
    }

    async fn stats(
        &self,
        _request: Request<StatsRequest>,
    ) -> Result<Response<StatsResponse>, Status> {
        let s = self.shard.stats();
        Ok(Response::new(StatsResponse {
            doc_count: s.doc_count,
            segment_count: s.segment_count,
            index_size_bytes: 0,
            shard_id: s.shard_id,
        }))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            healthy: true,
            status: "ok".into(),
        }))
    }
}
