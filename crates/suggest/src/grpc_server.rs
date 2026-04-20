use search_core::{Document, Value};
use search_proto::shard::shard_service_server::ShardService;
use search_proto::shard::{
    BulkIndexRequest, BulkIndexResponse, CatchUpRequest, DeleteRequest, DeleteResponse,
    DocumentProto, FlushRequest, FlushResponse, FullSyncRequest, GetDocsRequest, GetDocsResponse,
    HealthRequest, HealthResponse, IndexRequest, IndexResponse, ReindexRequest, ReindexResponse,
    ReplicateRequest, ReplicateResponse, SearchRequest as ProtoSearchRequest,
    SearchResponse as ProtoSearchResponse, StatsRequest, StatsResponse, SuggestEntry,
    SuggestRequest, SuggestResponse,
};
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::engine::SuggestShardEngine;

pub struct SuggestGrpcServer {
    engine: Arc<SuggestShardEngine>,
}

impl SuggestGrpcServer {
    pub fn new(engine: Arc<SuggestShardEngine>) -> Self {
        Self { engine }
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

#[tonic::async_trait]
impl ShardService for SuggestGrpcServer {
    type FullSyncStream = ReceiverStream<Result<DocumentProto, Status>>;
    type ReplicateCatchUpStream = ReceiverStream<Result<ReplicateRequest, Status>>;

    async fn suggest(
        &self,
        request: Request<SuggestRequest>,
    ) -> Result<Response<SuggestResponse>, Status> {
        let req = request.into_inner();
        let entries = self
            .engine
            .suggest(&req.prefix, &req.field, req.limit as usize)
            .into_iter()
            .map(|t| SuggestEntry { term: t.term, score: t.score })
            .collect();
        Ok(Response::new(SuggestResponse { entries }))
    }

    async fn index(
        &self,
        request: Request<IndexRequest>,
    ) -> Result<Response<IndexResponse>, Status> {
        let proto_doc = request
            .into_inner()
            .document
            .ok_or_else(|| Status::invalid_argument("missing document"))?;
        self.engine.index(&proto_to_doc(proto_doc));
        Ok(Response::new(IndexResponse { success: true }))
    }

    async fn bulk_index(
        &self,
        request: Request<BulkIndexRequest>,
    ) -> Result<Response<BulkIndexResponse>, Status> {
        let docs: Vec<Document> = request
            .into_inner()
            .documents
            .into_iter()
            .map(proto_to_doc)
            .collect();
        let count = docs.len() as u32;
        self.engine.bulk(&docs);
        Ok(Response::new(BulkIndexResponse { indexed: count }))
    }

    async fn flush(
        &self,
        _request: Request<FlushRequest>,
    ) -> Result<Response<FlushResponse>, Status> {
        self.engine.flush();
        Ok(Response::new(FlushResponse { segments_created: 0 }))
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

    async fn stats(
        &self,
        _request: Request<StatsRequest>,
    ) -> Result<Response<StatsResponse>, Status> {
        Ok(Response::new(StatsResponse {
            doc_count: self.engine.term_count() as u64,
            segment_count: 0,
            index_size_bytes: 0,
            shard_id: self.engine.config().shard_id,
        }))
    }

    // --- All search-path RPCs return Unimplemented on suggest shards. ---

    async fn search(
        &self,
        _request: Request<ProtoSearchRequest>,
    ) -> Result<Response<ProtoSearchResponse>, Status> {
        Err(Status::unimplemented("suggest shard does not implement Search"))
    }

    async fn get_docs(
        &self,
        _request: Request<GetDocsRequest>,
    ) -> Result<Response<GetDocsResponse>, Status> {
        Err(Status::unimplemented("suggest shard does not implement GetDocs"))
    }

    async fn delete(
        &self,
        _request: Request<DeleteRequest>,
    ) -> Result<Response<DeleteResponse>, Status> {
        Err(Status::unimplemented("suggest shard does not implement Delete"))
    }

    async fn reindex(
        &self,
        _request: Request<ReindexRequest>,
    ) -> Result<Response<ReindexResponse>, Status> {
        Err(Status::unimplemented("suggest shard does not implement Reindex"))
    }

    async fn replicate(
        &self,
        _request: Request<ReplicateRequest>,
    ) -> Result<Response<ReplicateResponse>, Status> {
        Err(Status::unimplemented("suggest shard does not implement Replicate"))
    }

    async fn replicate_catch_up(
        &self,
        _request: Request<CatchUpRequest>,
    ) -> Result<Response<Self::ReplicateCatchUpStream>, Status> {
        Err(Status::unimplemented("suggest shard does not implement ReplicateCatchUp"))
    }

    async fn full_sync(
        &self,
        _request: Request<FullSyncRequest>,
    ) -> Result<Response<Self::FullSyncStream>, Status> {
        Err(Status::unimplemented("suggest shard does not implement FullSync"))
    }
}
