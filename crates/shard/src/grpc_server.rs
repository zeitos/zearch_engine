use crate::shard::ShardEngine;
use search_core::{Document, FilterValue, SearchRequest, SortOrder, SortSpec, Value};
use search_proto::shard::shard_service_server::ShardService;
use search_proto::shard::{
    filter_value, replicate_request, AggregationBucket, AggregationResult, BulkIndexRequest,
    BulkIndexResponse, CatchUpRequest, DeleteRequest, DeleteResponse, DocumentProto, FlushRequest,
    FlushResponse, FullSyncRequest, GetDocsRequest, GetDocsResponse, HealthRequest, HealthResponse,
    IndexRequest, IndexResponse, ReindexRequest, ReindexResponse, ReplicateRequest,
    ReplicateResponse, ScoredDocument, SearchRequest as ProtoSearchRequest,
    SearchResponse as ProtoSearchResponse, SortOrder as ProtoSortOrder, StatsRequest, StatsResponse,
    SuggestRequest, SuggestResponse,
};
use tokio_stream::wrappers::ReceiverStream;
use crate::wal::WalEntry;
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
        destination_zone: None,
        include_docs: true,
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

        let retrieval_mode = match resp.retrieval_mode {
            search_core::RetrievalMode::And => "and".to_string(),
            search_core::RetrievalMode::OrFallback => "or_fallback".to_string(),
        };

        Ok(Response::new(ProtoSearchResponse {
            hits,
            total_hits: resp.total_hits,
            aggregations,
            retrieval_mode,
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

    async fn flush(
        &self,
        _request: Request<FlushRequest>,
    ) -> Result<Response<FlushResponse>, Status> {
        let before = self.shard.stats().segment_count;
        self.shard.flush().map_err(|e| Status::internal(e.to_string()))?;
        let after = self.shard.stats().segment_count;
        Ok(Response::new(FlushResponse {
            segments_created: after.saturating_sub(before),
        }))
    }

    async fn reindex(
        &self,
        _request: Request<ReindexRequest>,
    ) -> Result<Response<ReindexResponse>, Status> {
        let stats = self.shard.reindex_self().map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ReindexResponse {
            shard_id: stats.shard_id,
            docs_reindexed: stats.docs_reindexed,
            elapsed_ms: stats.elapsed_ms,
        }))
    }

    async fn replicate(
        &self,
        request: Request<ReplicateRequest>,
    ) -> Result<Response<ReplicateResponse>, Status> {
        if !self.shard.is_replica() {
            return Err(Status::failed_precondition("replicate called on a primary shard"));
        }
        let req = request.into_inner();
        let entry = match req.operation {
            Some(replicate_request::Operation::IndexDoc(doc)) => WalEntry::Index(proto_to_doc(doc)),
            Some(replicate_request::Operation::DeleteDocId(id)) => WalEntry::Delete(id),
            Some(replicate_request::Operation::Flush(_)) => {
                self.shard.flush().map_err(|e| Status::internal(e.to_string()))?;
                return Ok(Response::new(ReplicateResponse { ok: true }));
            }
            None => return Ok(Response::new(ReplicateResponse { ok: true })),
        };
        self.shard.apply_replicated(entry).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ReplicateResponse { ok: true }))
    }

    type FullSyncStream = ReceiverStream<Result<DocumentProto, Status>>;
    type ReplicateCatchUpStream = ReceiverStream<Result<ReplicateRequest, Status>>;

    async fn replicate_catch_up(
        &self,
        request: Request<CatchUpRequest>,
    ) -> Result<Response<Self::ReplicateCatchUpStream>, Status> {
        let from_seq = request.into_inner().from_wal_seq;
        let records = self.shard
            .wal_records_since(from_seq)
            .map_err(|e| Status::internal(e.to_string()))?;

        let (tx, rx) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            for record in records {
                let operation = match record.entry {
                    WalEntry::Index(doc) => replicate_request::Operation::IndexDoc(DocumentProto {
                        id: doc.id, title: doc.title, description: doc.description,
                        price: doc.price, category: doc.category,
                        attributes: doc.attributes.into_iter()
                            .filter_map(|(k, v)| {
                                if let search_core::Value::String(s) = v { Some((k, s)) } else { None }
                            })
                            .collect(),
                    }),
                    WalEntry::Delete(id) => replicate_request::Operation::DeleteDocId(id),
                };
                let msg = ReplicateRequest { operation: Some(operation), wal_seq: record.seq };
                if tx.send(Ok(msg)).await.is_err() { break; }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn full_sync(
        &self,
        _request: Request<FullSyncRequest>,
    ) -> Result<Response<Self::FullSyncStream>, Status> {
        let docs = self.shard.get_all_live_docs();
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            for doc in docs {
                let proto = DocumentProto {
                    id: doc.id,
                    title: doc.title,
                    description: doc.description,
                    price: doc.price,
                    category: doc.category,
                    attributes: doc.attributes.into_iter()
                        .filter_map(|(k, v)| {
                            if let search_core::Value::String(s) = v { Some((k, s)) } else { None }
                        })
                        .collect(),
                };
                if tx.send(Ok(proto)).await.is_err() { break; }
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn suggest(
        &self,
        _request: Request<SuggestRequest>,
    ) -> Result<Response<SuggestResponse>, Status> {
        Err(Status::unimplemented("search shard does not implement Suggest"))
    }
}
