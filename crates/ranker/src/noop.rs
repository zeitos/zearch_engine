use crate::Ranker;
use search_core::{RankCandidate, RankedResult};

/// Returns candidates in their original BM25 order, unchanged.
pub struct NoopRanker;

#[async_trait::async_trait]
impl Ranker for NoopRanker {
    async fn rerank(
        &self,
        _query: &str,
        candidates: &[RankCandidate],
    ) -> search_core::Result<Vec<RankedResult>> {
        Ok(candidates
            .iter()
            .map(|c| RankedResult { doc_id: c.doc_id, score: c.bm25_score })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_candidates(scores: &[(u64, f32)]) -> Vec<RankCandidate> {
        scores
            .iter()
            .map(|&(id, score)| RankCandidate {
                doc_id: id,
                bm25_score: score,
                title: format!("Doc {id}"),
                category: "test".into(),
                price: 0.0,
                attributes: HashMap::new(),
            })
            .collect()
    }

    #[tokio::test]
    async fn test_noop_preserves_order() {
        let ranker = NoopRanker;
        let candidates = make_candidates(&[(1, 0.9), (2, 0.5), (3, 0.1)]);
        let results = ranker.rerank("query", &candidates).await.unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].doc_id, 1);
        assert_eq!(results[1].doc_id, 2);
        assert_eq!(results[2].doc_id, 3);
    }

    #[tokio::test]
    async fn test_noop_preserves_scores() {
        let ranker = NoopRanker;
        let candidates = make_candidates(&[(10, 2.5), (20, 1.0)]);
        let results = ranker.rerank("q", &candidates).await.unwrap();
        assert_eq!(results[0].score, 2.5);
        assert_eq!(results[1].score, 1.0);
    }

    #[tokio::test]
    async fn test_noop_empty_candidates() {
        let ranker = NoopRanker;
        let results = ranker.rerank("q", &[]).await.unwrap();
        assert!(results.is_empty());
    }
}
