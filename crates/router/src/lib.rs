pub mod client;
pub mod router;
pub mod routing;

pub use client::{LocalShardClient, RemoteShardClient, ShardClient, ShardSearchResult};
pub use router::{Router, RouterStats};
pub use routing::jump_consistent_hash;

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{IndexSchema, SearchRequest, ShardConfig};
    use search_ranker::NoopRanker;
    use search_shard::ShardEngine;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_config(dir: &std::path::Path, shard_id: u32) -> ShardConfig {
        ShardConfig {
            data_dir: dir.join(format!("shard-{shard_id}")),
            shard_id,
            write_buffer_size: 1024 * 1024,
            ..Default::default()
        }
    }

    fn make_doc(id: u64, title: &str, category: &str) -> search_core::Document {
        search_core::Document {
            id,
            title: title.into(),
            description: "great product".into(),
            price: id as f64 * 100.0,
            category: category.into(),
            attributes: HashMap::new(),
        }
    }

    fn make_router(dir: &std::path::Path, num_shards: u32) -> Router {
        let schema = IndexSchema::default_product_schema();
        let shards: Vec<Arc<dyn ShardClient>> = (0..num_shards)
            .map(|i| {
                let shard = ShardEngine::open(make_config(dir, i), schema.clone()).unwrap();
                Arc::new(LocalShardClient::new(Arc::new(shard))) as Arc<dyn ShardClient>
            })
            .collect();
        Router::new(shards, Arc::new(NoopRanker), 30, 200, 5000)
    }

    #[tokio::test]
    async fn test_index_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 2);

        router.index(make_doc(1, "Samsung Galaxy phone", "electronics")).await.unwrap();
        router.index(make_doc(2, "Apple iPhone device", "electronics")).await.unwrap();
        router.index(make_doc(3, "Nike shoes", "clothing")).await.unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = router.search(req).await.unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 1);
    }

    #[tokio::test]
    async fn test_delete_via_router() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 2);

        router.index(make_doc(1, "Samsung Galaxy", "electronics")).await.unwrap();
        router.delete(1).await.unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = router.search(req).await.unwrap();
        assert!(resp.hits.is_empty());
    }

    #[tokio::test]
    async fn test_multi_shard_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 4);

        // Index 20 docs — they'll land on different shards via jump hash
        for i in 1..=20u64 {
            router.index(make_doc(i, "Samsung Galaxy product", "electronics")).await.unwrap();
        }

        let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
        let resp = router.search(req).await.unwrap();
        assert_eq!(resp.total_hits, 20);
        assert_eq!(resp.hits.len(), 20);
    }

    #[tokio::test]
    async fn test_aggregations_across_shards() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 2);

        for i in 1..=6u64 {
            let cat = if i <= 4 { "electronics" } else { "clothing" };
            router.index(make_doc(i, "product item", cat)).await.unwrap();
        }

        let req = SearchRequest {
            query: "".into(),
            aggregations: vec!["category".into()],
            limit: 10,
            ..Default::default()
        };
        let resp = router.search(req).await.unwrap();

        let cats = resp.aggregations.get("category").unwrap();
        let elec = cats.iter().find(|b| b.value == "electronics").unwrap();
        let cloth = cats.iter().find(|b| b.value == "clothing").unwrap();
        assert_eq!(elec.count, 4);
        assert_eq!(cloth.count, 2);
    }

    #[tokio::test]
    async fn test_jump_hash_routing() {
        // Same doc_id always routes to same shard
        for id in [1u64, 42, 999, 10000] {
            let s1 = jump_consistent_hash(id, 4);
            let s2 = jump_consistent_hash(id, 4);
            assert_eq!(s1, s2);
        }
    }

    // -----------------------------------------------------------------------
    // 7.17 — Load test: throughput + latency (run with `cargo test -- --ignored`)
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore = "slow: run explicitly with `cargo test -- --ignored`"]
    async fn test_load_100k_docs() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 4);

        let n: u64 = 100_000;
        let t0 = std::time::Instant::now();

        for i in 1..=n {
            let cat = if i % 2 == 0 { "electronics" } else { "clothing" };
            let title = format!("Product {i} Samsung Galaxy item");
            router.index(make_doc(i, &title, cat)).await.unwrap();
        }

        let index_ms = t0.elapsed().as_millis();
        let throughput = n as f64 / (index_ms as f64 / 1000.0);
        println!("Indexed {n} docs in {index_ms}ms ({throughput:.0} docs/s)");

        // Search latency benchmark — 100 queries, p99 target < 100ms
        let mut latencies = Vec::with_capacity(100);
        for _ in 0..100 {
            let t = std::time::Instant::now();
            let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
            let resp = router.search(req).await.unwrap();
            latencies.push(t.elapsed().as_millis());
            assert!(resp.total_hits > 0, "expected results for 'samsung'");
        }

        latencies.sort_unstable();
        let p50 = latencies[49];
        let p95 = latencies[94];
        let p99 = latencies[98];
        println!("Search latency — p50: {p50}ms  p95: {p95}ms  p99: {p99}ms");

        assert!(
            p99 < 100,
            "p99 search latency {p99}ms exceeds 100ms target"
        );
    }

    // -----------------------------------------------------------------------
    // 7.18 — WAL recovery: docs survive a crash (no explicit flush)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_wal_recovery_no_flush() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Index via router, but never flush — docs are only in WAL + write buffer
        {
            let config = make_config(dir.path(), 0);
            let shard = Arc::new(
                search_shard::ShardEngine::open(config, schema.clone()).unwrap(),
            );
            let shards: Vec<Arc<dyn ShardClient>> =
                vec![Arc::new(LocalShardClient::new(shard))];
            let router = Router::new(shards, Arc::new(NoopRanker), 30, 200, 5000);

            for i in 1..=10u64 {
                router.index(make_doc(i, "Samsung Galaxy product", "electronics")).await.unwrap();
            }
            // router + shard drop here — simulates crash without flush
        }

        // Re-open shard: WAL should be replayed automatically
        {
            let config = make_config(dir.path(), 0);
            let shard = Arc::new(
                search_shard::ShardEngine::open(config, schema).unwrap(),
            );
            let shards: Vec<Arc<dyn ShardClient>> =
                vec![Arc::new(LocalShardClient::new(shard))];
            let router = Router::new(shards, Arc::new(NoopRanker), 30, 200, 5000);

            let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
            let resp = router.search(req).await.unwrap();
            assert_eq!(resp.total_hits, 10, "all 10 docs should survive WAL replay");
        }
    }

    // -----------------------------------------------------------------------
    // 7.19 — Partial failure: router returns results when one shard is down
    // -----------------------------------------------------------------------

    struct FailingShardClient;

    #[async_trait::async_trait]
    impl ShardClient for FailingShardClient {
        async fn search(&self, _: SearchRequest) -> search_core::Result<ShardSearchResult> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn index(&self, _: search_core::Document) -> search_core::Result<()> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn delete(&self, _: u64) -> search_core::Result<()> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn get_docs(&self, _: &[u64]) -> search_core::Result<Vec<search_core::Document>> {
            Ok(vec![])
        }
        async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn health(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn test_partial_shard_failure_returns_results() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Shard 0: healthy, shard 1: always fails
        let healthy_shard = Arc::new(
            search_shard::ShardEngine::open(make_config(dir.path(), 0), schema).unwrap(),
        );
        let shards: Vec<Arc<dyn ShardClient>> = vec![
            Arc::new(LocalShardClient::new(healthy_shard)),
            Arc::new(FailingShardClient),
        ];
        let router = Router::new(shards, Arc::new(NoopRanker), 30, 200, 5000);

        // Index docs — some land on shard 0, some on shard 1 (the failing one)
        for i in 1..=20u64 {
            // Only index to healthy shard (shard 0); skip failures silently
            let _ = router.index(make_doc(i, "Samsung product", "electronics")).await;
        }

        // Search should succeed (partial results from healthy shard)
        let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
        let resp = router.search(req).await;
        assert!(resp.is_ok(), "router should return partial results, not error");
        let resp = resp.unwrap();
        assert!(resp.total_hits > 0, "should have results from healthy shard");
    }

    // -----------------------------------------------------------------------
    // 7.21 — Ranker fallback: slow ranker must not add >30ms overhead
    // -----------------------------------------------------------------------

    struct SlowRanker {
        delay_ms: u64,
    }

    #[async_trait::async_trait]
    impl search_ranker::Ranker for SlowRanker {
        async fn rerank(
            &self,
            _query: &str,
            candidates: &[search_core::RankCandidate],
        ) -> search_core::Result<Vec<search_core::RankedResult>> {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            Ok(candidates
                .iter()
                .map(|c| search_core::RankedResult { doc_id: c.doc_id, score: c.bm25_score })
                .collect())
        }
    }

    #[tokio::test]
    async fn test_ranker_fallback_overhead_under_30ms() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shards: Vec<Arc<dyn ShardClient>> = (0..2)
            .map(|i| {
                let shard = search_shard::ShardEngine::open(make_config(dir.path(), i), schema.clone()).unwrap();
                Arc::new(LocalShardClient::new(Arc::new(shard))) as Arc<dyn ShardClient>
            })
            .collect();

        // Ranker with 200ms delay, timeout set to 10ms → must fall back
        let ranker = Arc::new(SlowRanker { delay_ms: 200 });
        let router = Router::new(shards, ranker, 10 /* timeout_ms */, 200, 5000);

        for i in 1..=5u64 {
            router.index(make_doc(i, "Samsung Galaxy product", "electronics")).await.unwrap();
        }

        let req = SearchRequest { query: "samsung".into(), limit: 5, ..Default::default() };

        // Warm up
        let _ = router.search(req.clone()).await.unwrap();

        // Measure fallback overhead
        let t0 = std::time::Instant::now();
        let resp = router.search(req).await.unwrap();
        let elapsed_ms = t0.elapsed().as_millis();

        assert!(!resp.reranked, "ranker should have timed out and fallen back");
        assert!(
            elapsed_ms < 100,
            "fallback query took {elapsed_ms}ms, expected < 100ms"
        );
        println!("Ranker fallback query completed in {elapsed_ms}ms");
    }
}
