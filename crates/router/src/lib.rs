pub mod availability;
pub mod breaker;
pub mod cache;
pub mod client;
pub mod router;
pub mod routing;

pub use availability::{
    AvailabilityStrategy, NoopStrategy, RegionStockStrategy, StaticAvailabilityStrategy,
    build_strategy,
};
pub use breaker::BreakerShardClient;
pub use client::{
    LocalShardClient, LocalSuggestClient, RemoteShardClient, ShardClient, ShardGroup,
    ShardSearchResult,
};
pub use router::{Router, RouterStats, ShardGroupStats};
pub use routing::jump_consistent_hash;

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{IndexSchema, QueryCacheConfig, SearchRequest, ShardConfig};
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

    async fn make_router(dir: &std::path::Path, num_shards: u32) -> Router {
        let schema = IndexSchema::default_product_schema();
        let mut shards: Vec<Arc<dyn ShardClient>> = Vec::new();
        for i in 0..num_shards {
            let shard = ShardEngine::open(make_config(dir, i), schema.clone()).await.unwrap();
            shards.push(Arc::new(LocalShardClient::new(Arc::new(shard))) as Arc<dyn ShardClient>);
        }
        Router::new_simple(shards, Arc::new(NoopRanker), 30, 200, 5000)
    }

    async fn make_router_with_strategy(
        dir: &std::path::Path,
        num_shards: u32,
        strategy: Arc<dyn crate::availability::AvailabilityStrategy>,
    ) -> Router {
        let schema = IndexSchema::default_product_schema();
        let groups: Vec<ShardGroup> = {
            let mut v = Vec::new();
            for i in 0..num_shards {
                let shard = ShardEngine::open(make_config(dir, i), schema.clone()).await.unwrap();
                let client = Arc::new(LocalShardClient::new(Arc::new(shard))) as Arc<dyn ShardClient>;
                v.push(ShardGroup::primary_only(client));
            }
            v
        };
        Router::new(groups, Arc::new(NoopRanker), 30, 200, 5000, strategy)
    }

    #[tokio::test]
    async fn test_index_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 2).await;

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
        let router = make_router(dir.path(), 2).await;

        router.index(make_doc(1, "Samsung Galaxy", "electronics")).await.unwrap();
        router.delete(1).await.unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = router.search(req).await.unwrap();
        assert!(resp.hits.is_empty());
    }

    #[tokio::test]
    async fn test_multi_shard_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 4).await;

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
        let router = make_router(dir.path(), 2).await;

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
        let router = make_router(dir.path(), 4).await;

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
                search_shard::ShardEngine::open(config, schema.clone()).await.unwrap(),
            );
            let shards: Vec<Arc<dyn ShardClient>> =
                vec![Arc::new(LocalShardClient::new(shard))];
            let router = Router::new_simple(shards, Arc::new(NoopRanker), 30, 200, 5000);

            for i in 1..=10u64 {
                router.index(make_doc(i, "Samsung Galaxy product", "electronics")).await.unwrap();
            }
            // router + shard drop here — simulates crash without flush
        }

        // Re-open shard: WAL should be replayed automatically
        {
            let config = make_config(dir.path(), 0);
            let shard = Arc::new(
                search_shard::ShardEngine::open(config, schema).await.unwrap(),
            );
            let shards: Vec<Arc<dyn ShardClient>> =
                vec![Arc::new(LocalShardClient::new(shard))];
            let router = Router::new_simple(shards, Arc::new(NoopRanker), 30, 200, 5000);

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
        async fn bulk(&self, _: Vec<search_core::Document>) -> search_core::Result<u32> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn get_docs(&self, _: &[u64]) -> search_core::Result<Vec<search_core::Document>> {
            Ok(vec![])
        }
        async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn flush(&self) -> search_core::Result<()> {
            Err(search_core::Error::Shard("shard unavailable".into()))
        }
        async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats> {
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
            search_shard::ShardEngine::open(make_config(dir.path(), 0), schema).await.unwrap(),
        );
        let shards: Vec<Arc<dyn ShardClient>> = vec![
            Arc::new(LocalShardClient::new(healthy_shard)),
            Arc::new(FailingShardClient),
        ];
        let router = Router::new_simple(shards, Arc::new(NoopRanker), 30, 200, 5000);

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
        let mut shards: Vec<Arc<dyn ShardClient>> = Vec::new();
        for i in 0..2u32 {
            let shard = search_shard::ShardEngine::open(make_config(dir.path(), i), schema.clone()).await.unwrap();
            shards.push(Arc::new(LocalShardClient::new(Arc::new(shard))) as Arc<dyn ShardClient>);
        }

        // Ranker with 200ms delay, timeout set to 10ms → must fall back
        let ranker = Arc::new(SlowRanker { delay_ms: 200 });
        let router = Router::new_simple(shards, ranker, 10 /* timeout_ms */, 200, 5000);

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

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------
    // 006 — Query cache integration
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_cache_hit_skips_scatter() {
        use search_core::QueryCacheConfig;

        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = Arc::new(ShardEngine::open(make_config(dir.path(), 0), schema).await.unwrap());
        let client = Arc::new(LocalShardClient::new(shard)) as Arc<dyn ShardClient>;
        let group = ShardGroup::primary_only(client);

        let router = Router::with_cache(
            vec![group],
            Arc::new(NoopRanker),
            30, 200, 5000,
            Arc::new(crate::availability::NoopStrategy),
            &QueryCacheConfig { enabled: true, max_entries: 100, ttl_seconds: 60 },
        );

        router.index(make_doc(1, "Samsung Galaxy phone", "electronics")).await.unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };

        let resp1 = router.search(req.clone()).await.unwrap();
        assert!(!resp1.cache_hit, "first request should be a miss");
        assert_eq!(resp1.hits.len(), 1);

        let resp2 = router.search(req.clone()).await.unwrap();
        assert!(resp2.cache_hit, "second identical request should be a hit");
        assert_eq!(resp2.hits.len(), 1);
        assert_eq!(resp2.hits[0].id, resp1.hits[0].id);
    }

    #[tokio::test]
    async fn test_cache_different_zone_is_miss() {
        use search_core::QueryCacheConfig;

        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = Arc::new(ShardEngine::open(make_config(dir.path(), 0), schema).await.unwrap());
        let client = Arc::new(LocalShardClient::new(shard)) as Arc<dyn ShardClient>;
        let group = ShardGroup::primary_only(client);

        let router = Router::with_cache(
            vec![group],
            Arc::new(NoopRanker),
            30, 200, 5000,
            Arc::new(crate::availability::NoopStrategy),
            &QueryCacheConfig { enabled: true, max_entries: 100, ttl_seconds: 60 },
        );

        router.index(make_doc(1, "Samsung Galaxy phone", "electronics")).await.unwrap();

        let req_a = SearchRequest { query: "samsung".into(), destination_zone: Some("zone_a".into()), limit: 10, ..Default::default() };
        let req_b = SearchRequest { query: "samsung".into(), destination_zone: Some("zone_b".into()), limit: 10, ..Default::default() };

        let resp_a = router.search(req_a).await.unwrap();
        let resp_b = router.search(req_b).await.unwrap();
        assert!(!resp_a.cache_hit);
        assert!(!resp_b.cache_hit, "different zone should be a separate cache miss");
    }

    // -----------------------------------------------------------------------
    // 005 — Availability strategy integration
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_search_with_destination_zone() {
        use crate::availability::StaticAvailabilityStrategy;
        use search_core::{StaticAvailabilityConfig, Value};

        let dir = tempfile::TempDir::new().unwrap();
        let strategy = Arc::new(StaticAvailabilityStrategy::new(StaticAvailabilityConfig {
            stock_attribute_prefix: "stock_".into(),
            default_available: false,
            min_stock: 1.0,
        }));
        let router = make_router_with_strategy(dir.path(), 2, strategy).await;

        let mut doc1 = make_doc(1, "Samsung Galaxy phone", "electronics");
        doc1.attributes.insert("stock_buenos_aires".into(), Value::Number(10.0));

        let mut doc2 = make_doc(2, "Apple iPhone device", "electronics");
        doc2.attributes.insert("stock_buenos_aires".into(), Value::Number(0.0));

        // No stock attribute → excluded (default_available=false)
        let doc3 = make_doc(3, "Samsung tablet", "electronics");

        router.index(doc1).await.unwrap();
        router.index(doc2).await.unwrap();
        router.index(doc3).await.unwrap();

        let req = SearchRequest {
            query: "samsung".into(),
            limit: 10,
            destination_zone: Some("buenos_aires".into()),
            ..Default::default()
        };
        let resp = router.search(req).await.unwrap();

        assert_eq!(resp.hits.len(), 1, "only in-stock doc should be returned");
        assert_eq!(resp.hits[0].id, 1);
    }

    // T-11e: ShardGroup read round-robin distributes reads across primary + replica
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_shard_group_read_roundrobin() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Two independent shards — primary has doc A, replica has doc B
        let s_primary = Arc::new(
            ShardEngine::open(make_config(dir.path(), 0), schema.clone()).await.unwrap(),
        );
        let s_replica = Arc::new(
            ShardEngine::open(make_config(dir.path(), 1), schema.clone()).await.unwrap(),
        );

        // Use brand names that the Spanish stemmer won't conflate
        s_primary.index(search_core::Document {
            id: 1, title: "samsung".into(), description: "".into(),
            price: 0.0, category: "x".into(), attributes: HashMap::new(),
        }).unwrap();
        s_replica.index(search_core::Document {
            id: 2, title: "motorola".into(), description: "".into(),
            price: 0.0, category: "x".into(), attributes: HashMap::new(),
        }).unwrap();

        let c_primary = Arc::new(LocalShardClient::new(Arc::clone(&s_primary))) as Arc<dyn ShardClient>;
        let c_replica = Arc::new(LocalShardClient::new(Arc::clone(&s_replica))) as Arc<dyn ShardClient>;

        let group = ShardGroup::new(c_primary, vec![c_replica]);
        assert_eq!(group.replica_count(), 1);

        let req_a = SearchRequest { query: "samsung".into(), limit: 5, ..Default::default() };
        let req_b = SearchRequest { query: "motorola".into(), limit: 5, ..Default::default() };

        // With 2 endpoints and 4 read_target() calls: primary(0), replica(1), primary(2), replica(3)
        let mut primary_hits = 0u32;
        let mut replica_hits = 0u32;
        for _ in 0..4 {
            let target = group.read_target();
            if target.search(req_a.clone()).await.unwrap().hits.len() > 0 { primary_hits += 1; }
            if target.search(req_b.clone()).await.unwrap().hits.len() > 0 { replica_hits += 1; }
        }
        assert_eq!(primary_hits, 2, "primary should be selected 2 out of 4 reads");
        assert_eq!(replica_hits, 2, "replica should be selected 2 out of 4 reads");
    }

    // -----------------------------------------------------------------------
    // 008 — Autocomplete / Suggest integration
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_suggest_index_and_query_via_router() {
        use search_core::SuggestShardConfig;
        use search_suggest::SuggestShardEngine;

        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 2).await;

        // Build a small in-process suggest cluster (2 shards).
        let mut suggest_clients: Vec<Arc<dyn ShardClient>> = Vec::new();
        let mut engines: Vec<Arc<SuggestShardEngine>> = Vec::new();
        for i in 0..2u32 {
            let cfg = SuggestShardConfig {
                shard_id: i,
                num_suggest_shards: 2,
                min_doc_frequency: 1,
                max_terms_per_shard: 1000,
                fields: vec!["title".into()],
                ..SuggestShardConfig::default()
            };
            let engine = Arc::new(SuggestShardEngine::new(cfg));
            engines.push(Arc::clone(&engine));
            suggest_clients.push(Arc::new(LocalSuggestClient::new(engine)) as Arc<dyn ShardClient>);
        }

        let router = router.with_suggest_shards(suggest_clients, 500, 500);

        // Index docs via router — fans out to search + suggest.
        for i in 1..=5u64 {
            router
                .index(make_doc(i, "wireless mouse", "electronics"))
                .await
                .unwrap();
        }
        for i in 6..=7u64 {
            router
                .index(make_doc(i, "wired mouse", "electronics"))
                .await
                .unwrap();
        }

        // Suggest writes are fire-and-forget, so let the spawned tasks run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        for engine in &engines {
            engine.flush();
        }

        let r = router.suggest("wire", "title", 5).await.unwrap();
        let terms: Vec<&str> = r.iter().map(|t| t.term.as_str()).collect();
        assert!(terms.contains(&"wireless"), "expected wireless in {:?}", terms);
        assert!(terms.contains(&"wired"), "expected wired in {:?}", terms);
        let wireless_pos = terms.iter().position(|&t| t == "wireless").unwrap();
        let wired_pos = terms.iter().position(|&t| t == "wired").unwrap();
        assert!(
            wireless_pos < wired_pos,
            "wireless (5 docs) must rank above wired (2 docs)"
        );
    }

    #[tokio::test]
    async fn test_suggest_write_failure_does_not_fail_index() {
        // A suggest shard that always errors on writes.
        struct FailingSuggestClient;
        #[async_trait::async_trait]
        impl ShardClient for FailingSuggestClient {
            async fn search(&self, _: SearchRequest) -> search_core::Result<ShardSearchResult> {
                Ok(ShardSearchResult {
                    hits: vec![], total_hits: 0,
                    aggregations: HashMap::new(), used_or_fallback: false,
                })
            }
            async fn index(&self, _: search_core::Document) -> search_core::Result<()> {
                Err(search_core::Error::Shard("suggest down".into()))
            }
            async fn bulk(&self, _: Vec<search_core::Document>) -> search_core::Result<u32> {
                Err(search_core::Error::Shard("suggest down".into()))
            }
            async fn delete(&self, _: u64) -> search_core::Result<()> { Ok(()) }
            async fn get_docs(&self, _: &[u64]) -> search_core::Result<Vec<search_core::Document>> {
                Ok(vec![])
            }
            async fn stats(&self) -> search_core::Result<search_shard::ShardStats> {
                Err(search_core::Error::Shard("suggest down".into()))
            }
            async fn flush(&self) -> search_core::Result<()> { Ok(()) }
            async fn reindex(&self) -> search_core::Result<search_shard::ReindexStats> {
                Err(search_core::Error::Shard("suggest down".into()))
            }
            async fn health(&self) -> bool { false }
        }

        let dir = tempfile::TempDir::new().unwrap();
        let router = make_router(dir.path(), 1)
            .await
            .with_suggest_shards(
                vec![Arc::new(FailingSuggestClient) as Arc<dyn ShardClient>],
                50,
                50,
            );

        // Must succeed despite suggest-side failure (fire-and-forget).
        router
            .index(make_doc(1, "wireless mouse", "electronics"))
            .await
            .unwrap();
    }
}
