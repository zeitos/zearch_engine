use crate::Ranker;
use search_core::{CircuitBreaker, CircuitBreakerConfig, RankCandidate, RankedResult};
use std::sync::Arc;

/// Ranker wrapped in a circuit breaker.
/// When the circuit is open, returns Err immediately so `rerank_with_fallback`
/// degrades to BM25 ordering instead of waiting for the timeout.
pub struct BreakerRanker {
    inner: Arc<dyn Ranker>,
    breaker: Arc<CircuitBreaker>,
}

impl BreakerRanker {
    pub fn new(inner: Arc<dyn Ranker>, config: &CircuitBreakerConfig) -> Self {
        Self {
            inner,
            breaker: Arc::new(CircuitBreaker::new(config, "ranker_circuit_open_total")),
        }
    }
}

#[async_trait::async_trait]
impl Ranker for BreakerRanker {
    async fn rerank(
        &self,
        query: &str,
        candidates: &[RankCandidate],
    ) -> search_core::Result<Vec<RankedResult>> {
        if self.breaker.is_open() {
            metrics::counter!("ranker_circuit_short_circuit_total").increment(1);
            return Err(search_core::Error::Internal("ranker circuit open".into()));
        }
        match self.inner.rerank(query, candidates).await {
            Ok(r) => {
                self.breaker.record_success();
                Ok(r)
            }
            Err(e) => {
                self.breaker.record_failure();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoopRanker;
    use std::collections::HashMap;

    fn cfg(threshold: u32) -> CircuitBreakerConfig {
        CircuitBreakerConfig { enabled: true, failure_threshold: threshold, recovery_ms: 10_000 }
    }

    fn cands() -> Vec<RankCandidate> {
        vec![RankCandidate {
            doc_id: 1,
            bm25_score: 0.5,
            title: "x".into(),
            category: "y".into(),
            price: 0.0,
            attributes: HashMap::new(),
        }]
    }

    struct AlwaysFail;
    #[async_trait::async_trait]
    impl Ranker for AlwaysFail {
        async fn rerank(&self, _q: &str, _c: &[RankCandidate]) -> search_core::Result<Vec<RankedResult>> {
            Err(search_core::Error::Internal("boom".into()))
        }
    }

    #[tokio::test]
    async fn opens_after_failures_then_short_circuits() {
        let r = BreakerRanker::new(Arc::new(AlwaysFail), &cfg(2));
        assert!(r.rerank("q", &cands()).await.is_err());
        assert!(r.rerank("q", &cands()).await.is_err());
        assert!(r.breaker.is_open());
        assert!(r.rerank("q", &cands()).await.is_err());
    }

    #[tokio::test]
    async fn passes_through_when_closed() {
        let r = BreakerRanker::new(Arc::new(NoopRanker), &cfg(3));
        assert!(r.rerank("q", &cands()).await.is_ok());
    }
}
