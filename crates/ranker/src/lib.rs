pub mod breaker;
pub mod factory;
pub mod grpc;
pub mod noop;
pub mod wasm;

use search_core::{RankCandidate, RankedResult};
use std::time::Duration;

pub use breaker::BreakerRanker;
pub use factory::RankerFactory;
pub use grpc::GrpcRanker;
pub use noop::NoopRanker;
pub use wasm::WasmRanker;

/// Pluggable re-ranker trait.
#[async_trait::async_trait]
pub trait Ranker: Send + Sync {
    async fn rerank(
        &self,
        query: &str,
        candidates: &[RankCandidate],
    ) -> search_core::Result<Vec<RankedResult>>;
}

/// Re-rank with a timeout. Falls back to BM25 order on timeout or error.
/// Returns (results, reranked) where reranked=true means the ranker succeeded.
pub async fn rerank_with_fallback(
    ranker: &dyn Ranker,
    query: &str,
    candidates: &[RankCandidate],
    timeout: Duration,
) -> (Vec<RankedResult>, bool) {
    let start = std::time::Instant::now();
    metrics::counter!("ranker_requests_total").increment(1);

    let fut = ranker.rerank(query, candidates);
    let result = match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(results)) => {
            metrics::histogram!("ranker_duration_seconds").record(start.elapsed().as_secs_f64());
            (results, true)
        }
        _ => {
            metrics::counter!("ranker_fallbacks_total").increment(1);
            let fallback = candidates
                .iter()
                .map(|c| RankedResult { doc_id: c.doc_id, score: c.bm25_score })
                .collect();
            (fallback, false)
        }
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{RankCandidate, RankedResult};
    use std::collections::HashMap;

    fn make_candidates(ids_scores: &[(u64, f32)]) -> Vec<RankCandidate> {
        ids_scores
            .iter()
            .map(|&(id, score)| RankCandidate {
                doc_id: id,
                bm25_score: score,
                title: format!("doc{id}"),
                category: "test".into(),
                price: 0.0,
                attributes: HashMap::new(),
            })
            .collect()
    }

    struct SlowRanker;

    #[async_trait::async_trait]
    impl Ranker for SlowRanker {
        async fn rerank(
            &self,
            _query: &str,
            candidates: &[RankCandidate],
        ) -> search_core::Result<Vec<RankedResult>> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(candidates.iter().map(|c| RankedResult { doc_id: c.doc_id, score: 99.0 }).collect())
        }
    }

    #[tokio::test]
    async fn test_fallback_on_timeout() {
        let ranker = SlowRanker;
        let candidates = make_candidates(&[(1, 0.9), (2, 0.5)]);
        let (results, reranked) =
            rerank_with_fallback(&ranker, "q", &candidates, Duration::from_millis(10)).await;
        assert!(!reranked);
        // Fallback preserves BM25 order and scores
        assert_eq!(results[0].doc_id, 1);
        assert_eq!(results[0].score, 0.9);
    }

    #[tokio::test]
    async fn test_no_fallback_when_fast() {
        let ranker = NoopRanker;
        let candidates = make_candidates(&[(1, 0.9)]);
        let (results, reranked) =
            rerank_with_fallback(&ranker, "q", &candidates, Duration::from_millis(100)).await;
        assert!(reranked);
        assert_eq!(results.len(), 1);
    }
}
