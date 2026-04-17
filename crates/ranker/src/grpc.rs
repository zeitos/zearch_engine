use crate::Ranker;
use search_core::{RankCandidate, RankedResult};
use search_proto::ranker::ranker_service_client::RankerServiceClient;
use search_proto::ranker::{RankCandidate as ProtoCandidate, RerankRequest};
use tonic::transport::Channel;

pub struct GrpcRanker {
    endpoint: String,
}

impl GrpcRanker {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self { endpoint: endpoint.into() }
    }

    async fn connect(&self) -> search_core::Result<RankerServiceClient<Channel>> {
        RankerServiceClient::connect(self.endpoint.clone())
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))
    }
}

#[async_trait::async_trait]
impl Ranker for GrpcRanker {
    async fn rerank(
        &self,
        query: &str,
        candidates: &[RankCandidate],
    ) -> search_core::Result<Vec<RankedResult>> {
        let mut client = self.connect().await?;

        let proto_candidates: Vec<ProtoCandidate> = candidates
            .iter()
            .map(|c| ProtoCandidate {
                doc_id: c.doc_id,
                bm25_score: c.bm25_score,
                title: c.title.clone(),
                category: c.category.clone(),
                price: c.price,
                attributes: c
                    .attributes
                    .iter()
                    .filter_map(|(k, v)| {
                        if let search_core::Value::String(s) = v {
                            Some((k.clone(), s.clone()))
                        } else {
                            None
                        }
                    })
                    .collect(),
            })
            .collect();

        let request = tonic::Request::new(RerankRequest {
            query: query.to_string(),
            candidates: proto_candidates,
        });

        let response = client
            .rerank(request)
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?;

        let results = response
            .into_inner()
            .results
            .into_iter()
            .map(|r| RankedResult { doc_id: r.doc_id, score: r.score })
            .collect();

        Ok(results)
    }
}
