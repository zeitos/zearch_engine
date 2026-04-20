use crate::client::{ShardClient, ShardSearchResult};
use search_core::{CircuitBreaker, CircuitBreakerConfig, Document, SearchRequest};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// BreakerShardClient — wraps any ShardClient with a circuit breaker
// ---------------------------------------------------------------------------

pub struct BreakerShardClient {
    inner: Arc<dyn ShardClient>,
    breaker: Arc<CircuitBreaker>,
}

impl BreakerShardClient {
    pub fn new(inner: Arc<dyn ShardClient>, config: &CircuitBreakerConfig) -> Self {
        Self {
            inner,
            breaker: Arc::new(CircuitBreaker::new(config, "shard_circuit_open_total")),
        }
    }

    fn record<T>(&self, result: search_core::Result<T>) -> search_core::Result<T> {
        if result.is_ok() {
            self.breaker.record_success();
        } else {
            self.breaker.record_failure();
        }
        result
    }
}

#[async_trait::async_trait]
impl ShardClient for BreakerShardClient {
    fn is_circuit_open(&self) -> bool {
        self.breaker.is_open()
    }

    async fn search(&self, request: SearchRequest) -> search_core::Result<ShardSearchResult> {
        self.record(self.inner.search(request).await)
    }

    async fn get_docs(&self, doc_ids: &[u64]) -> search_core::Result<Vec<Document>> {
        self.record(self.inner.get_docs(doc_ids).await)
    }

    async fn index(&self, doc: Document) -> search_core::Result<()> {
        self.inner.index(doc).await
    }

    async fn bulk(&self, docs: Vec<Document>) -> search_core::Result<u32> {
        self.inner.bulk(docs).await
    }

    async fn delete(&self, doc_id: u64) -> search_core::Result<()> {
        self.inner.delete(doc_id).await
    }

    async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
        self.inner.stats().await
    }

    async fn flush(&self) -> search_core::Result<()> {
        self.inner.flush().await
    }

    async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats> {
        self.inner.reindex().await
    }

    async fn health(&self) -> bool {
        self.inner.health().await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn config(threshold: u32, recovery_ms: u64) -> CircuitBreakerConfig {
        CircuitBreakerConfig { enabled: true, failure_threshold: threshold, recovery_ms }
    }

    #[test]
    fn test_closed_by_default() {
        let cb = CircuitBreaker::new(&config(3, 10_000), "test");
        assert!(!cb.is_open());
    }

    #[test]
    fn test_opens_after_threshold() {
        let cb = CircuitBreaker::new(&config(3, 10_000), "test");
        cb.record_failure();
        cb.record_failure();
        assert!(!cb.is_open());
        cb.record_failure();
        assert!(cb.is_open());
    }

    #[test]
    fn test_closes_on_success() {
        let cb = CircuitBreaker::new(&config(2, 10_000), "test");
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        cb.record_success();
        assert!(!cb.is_open());
    }

    #[test]
    fn test_half_open_after_recovery_window() {
        let cb = CircuitBreaker::new(&config(1, 0), "test");
        cb.record_failure();
        assert!(!cb.is_open());
    }
}
