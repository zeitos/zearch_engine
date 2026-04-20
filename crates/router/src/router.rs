use crate::availability::AvailabilityStrategy;
use crate::cache::{CachedResponse, QueryCache, QueryCacheKey};
use crate::client::{ShardGroup, ShardSearchResult};
use crate::routing::jump_consistent_hash;
use search_core::{
    AggregationBucket, Document, QueryCacheConfig, RankCandidate, RetrievalMode, SearchRequest,
    SearchResponse, SearchHit,
};
use search_ranker::{rerank_with_fallback, Ranker};
use search_shard::ReindexStats;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub struct ShardGroupStats {
    pub primary: search_shard::ShardStats,
    pub replicas: Vec<search_shard::ShardStats>,
}

pub struct RouterStats {
    pub shards: Vec<ShardGroupStats>,
}

pub struct Router {
    shards: Vec<ShardGroup>,
    ranker: Arc<dyn Ranker>,
    ranker_timeout: Duration,
    ranker_candidates: usize,
    query_timeout: Duration,
    availability: Arc<dyn AvailabilityStrategy>,
    cache: Option<QueryCache>,
    suggest_shards: Vec<Arc<dyn crate::client::ShardClient>>,
    suggest_write_timeout: Duration,
    suggest_query_timeout: Duration,
}

impl Router {
    pub fn new(
        shards: Vec<ShardGroup>,
        ranker: Arc<dyn Ranker>,
        ranker_timeout_ms: u64,
        ranker_candidates: usize,
        query_timeout_ms: u64,
        availability: Arc<dyn AvailabilityStrategy>,
    ) -> Self {
        Self::with_cache(shards, ranker, ranker_timeout_ms, ranker_candidates, query_timeout_ms, availability, &QueryCacheConfig::default())
    }

    pub fn with_cache(
        shards: Vec<ShardGroup>,
        ranker: Arc<dyn Ranker>,
        ranker_timeout_ms: u64,
        ranker_candidates: usize,
        query_timeout_ms: u64,
        availability: Arc<dyn AvailabilityStrategy>,
        cache_config: &QueryCacheConfig,
    ) -> Self {
        Self {
            shards,
            ranker,
            ranker_timeout: Duration::from_millis(ranker_timeout_ms),
            ranker_candidates,
            query_timeout: Duration::from_millis(query_timeout_ms),
            availability,
            cache: QueryCache::build(cache_config),
            suggest_shards: Vec::new(),
            suggest_write_timeout: Duration::from_millis(200),
            suggest_query_timeout: Duration::from_millis(100),
        }
    }

    /// Attach an independent suggest cluster. Empty disables autocomplete.
    pub fn with_suggest_shards(
        mut self,
        shards: Vec<Arc<dyn crate::client::ShardClient>>,
        write_timeout_ms: u64,
        query_timeout_ms: u64,
    ) -> Self {
        self.suggest_shards = shards;
        self.suggest_write_timeout = Duration::from_millis(write_timeout_ms);
        self.suggest_query_timeout = Duration::from_millis(query_timeout_ms);
        self
    }

    /// Convenience constructor for shards without replicas (backward compat + tests).
    pub fn new_simple(
        shards: Vec<Arc<dyn crate::client::ShardClient>>,
        ranker: Arc<dyn Ranker>,
        ranker_timeout_ms: u64,
        ranker_candidates: usize,
        query_timeout_ms: u64,
    ) -> Self {
        let groups = shards.into_iter().map(ShardGroup::primary_only).collect();
        Self::new(
            groups,
            ranker,
            ranker_timeout_ms,
            ranker_candidates,
            query_timeout_ms,
            Arc::new(crate::availability::NoopStrategy),
        )
    }

    /// Fan-out search to all shards, merge, optionally rerank, paginate, fetch docs.
    #[tracing::instrument(
        skip_all,
        fields(
            query = %request.query,
            limit = request.limit,
            has_filters = !request.filters.is_empty(),
            destination_zone = request.destination_zone.as_deref().unwrap_or(""),
            cache_hit = tracing::field::Empty,
            total_hits = tracing::field::Empty,
            healthy_shards = tracing::field::Empty,
            reranked = tracing::field::Empty,
        )
    )]
    pub async fn search(&self, request: SearchRequest) -> search_core::Result<SearchResponse> {
        let start = std::time::Instant::now();
        metrics::counter!("search_queries_total").increment(1);

        // 0. Cache lookup
        let cache_key = self.cache.as_ref().map(|_| QueryCacheKey::from_request(&request));
        if let (Some(cache), Some(key)) = (&self.cache, &cache_key) {
            if let Some(cached) = cache.get(key).await {
                tracing::Span::current().record("cache_hit", true);
                return Ok(self.respond_from_cache(&request, &cached, start).await);
            }
        }
        tracing::Span::current().record("cache_hit", false);

        // 1. Scatter — inflate fetch size to compensate for availability filtering loss
        let scatter_limit = self.ranker_candidates * self.availability.candidate_multiplier();
        let mut shard_req = request.clone();
        shard_req.offset = 0;
        shard_req.limit = scatter_limit;

        let futures: Vec<_> = self
            .shards
            .iter()
            .map(|group| {
                let req = shard_req.clone();
                let shard = Arc::clone(group.read_target());
                let timeout = self.query_timeout;
                async move {
                    tokio::time::timeout(timeout, shard.search(req))
                        .await
                        .unwrap_or_else(|_| Err(search_core::Error::Shard("shard query timeout".into())))
                }
            })
            .collect();

        let shard_results: Vec<search_core::Result<ShardSearchResult>> =
            futures::future::join_all(futures).await;

        // 2. Gather — merge scored hits across shards
        let mut all_hits: Vec<(u64, f32)> = Vec::new();
        let mut total_hits: u64 = 0;
        let mut merged_aggs: HashMap<String, HashMap<String, u64>> = HashMap::new();
        let mut healthy_shards = 0usize;
        let mut any_or_fallback = false;

        for result in shard_results {
            match result {
                Ok(shard_result) => {
                    all_hits.extend(shard_result.hits);
                    total_hits += shard_result.total_hits;
                    for (field, counts) in shard_result.aggregations {
                        let bucket = merged_aggs.entry(field).or_default();
                        for (value, count) in counts {
                            *bucket.entry(value).or_default() += count;
                        }
                    }
                    if shard_result.used_or_fallback {
                        any_or_fallback = true;
                    }
                    healthy_shards += 1;
                }
                Err(e) => {
                    tracing::warn!("Shard search failed: {e}");
                }
            }
        }

        tracing::Span::current().record("healthy_shards", healthy_shards);
        if healthy_shards == 0 {
            return Err(search_core::Error::Shard("all shards failed".into()));
        }

        // 3. Sort globally by score descending
        all_hits.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // 4. Fetch candidate docs — used by both ranker and availability strategy
        let candidate_docs = if !all_hits.is_empty() {
            let ids: Vec<u64> = all_hits.iter().map(|(id, _)| *id).collect();
            self.fetch_docs(&ids).await
        } else {
            vec![]
        };
        let candidate_doc_map: HashMap<u64, &Document> =
            candidate_docs.iter().map(|d| (d.id, d)).collect();

        // 5. Optional re-ranking on top-N candidates
        let (mut final_hits, reranked) = if all_hits.is_empty() {
            (all_hits, false)
        } else {
            let rank_candidates: Vec<RankCandidate> = all_hits
                .iter()
                .filter_map(|(id, score)| {
                    let doc = candidate_doc_map.get(id)?;
                    Some(RankCandidate {
                        doc_id: *id,
                        bm25_score: *score,
                        title: doc.title.clone(),
                        category: doc.category.clone(),
                        price: doc.price,
                        attributes: doc.attributes.clone(),
                    })
                })
                .collect();

            let (ranked, did_rerank) =
                rerank_with_fallback(self.ranker.as_ref(), &request.query, &rank_candidates, self.ranker_timeout).await;

            let reranked_hits: Vec<(u64, f32)> =
                ranked.into_iter().map(|r| (r.doc_id, r.score)).collect();
            (reranked_hits, did_rerank)
        };

        // 6. Apply availability filter (post-rerank, pre-pagination)
        if let Some(zone) = &request.destination_zone {
            if !zone.is_empty() {
                let t0 = std::time::Instant::now();
                self.availability.apply(&mut final_hits, &candidate_doc_map, zone).await;
                metrics::histogram!("availability_filter_duration_seconds")
                    .record(t0.elapsed().as_secs_f64());
            }
        }

        let retrieval_mode =
            if any_or_fallback { RetrievalMode::OrFallback } else { RetrievalMode::And };

        let aggregations: HashMap<String, Vec<AggregationBucket>> = merged_aggs
            .into_iter()
            .map(|(field, counts)| {
                let mut buckets: Vec<AggregationBucket> = counts
                    .into_iter()
                    .map(|(value, count)| AggregationBucket { value, count })
                    .collect();
                buckets.sort_by(|a, b| b.count.cmp(&a.count));
                (field, buckets)
            })
            .collect();

        // 7. Store in cache before pagination (full candidate list)
        if let (Some(cache), Some(key)) = (&self.cache, cache_key) {
            cache.insert(key, CachedResponse {
                hits: final_hits.clone(),
                total_hits,
                aggregations: aggregations.clone(),
                reranked,
                retrieval_mode,
            }).await;
        }

        // 8. Paginate
        let page_hits: Vec<(u64, f32)> = final_hits
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect();

        // 9. Resolve full docs only when requested
        let hits = self.build_hits(page_hits, &candidate_doc_map, request.include_docs).await;

        let elapsed = start.elapsed();
        metrics::histogram!("search_query_duration_seconds").record(elapsed.as_secs_f64());
        tracing::Span::current().record("total_hits", total_hits);
        tracing::Span::current().record("reranked", reranked);
        if reranked {
            metrics::counter!("ranker_rerank_total").increment(1);
        }

        Ok(SearchResponse {
            hits,
            total_hits,
            aggregations,
            reranked,
            took_ms: elapsed.as_millis() as u64,
            retrieval_mode,
            cache_hit: false,
        })
    }

    async fn respond_from_cache(
        &self,
        request: &SearchRequest,
        cached: &CachedResponse,
        start: std::time::Instant,
    ) -> SearchResponse {
        let page_hits: Vec<(u64, f32)> = cached.hits
            .iter()
            .skip(request.offset)
            .take(request.limit)
            .copied()
            .collect();

        let empty_map: HashMap<u64, &Document> = HashMap::new();
        let hits = self.build_hits(page_hits, &empty_map, request.include_docs).await;

        let elapsed = start.elapsed();
        metrics::histogram!("search_query_duration_seconds").record(elapsed.as_secs_f64());

        SearchResponse {
            hits,
            total_hits: cached.total_hits,
            aggregations: cached.aggregations.clone(),
            reranked: cached.reranked,
            took_ms: elapsed.as_millis() as u64,
            retrieval_mode: cached.retrieval_mode,
            cache_hit: true,
        }
    }

    async fn build_hits(
        &self,
        page_hits: Vec<(u64, f32)>,
        candidate_doc_map: &HashMap<u64, &Document>,
        include_docs: bool,
    ) -> Vec<SearchHit> {
        if !include_docs {
            return page_hits
                .into_iter()
                .map(|(id, score)| SearchHit { id, score, document: None })
                .collect();
        }
        let page_ids: Vec<u64> = page_hits.iter().map(|(id, _)| *id).collect();
        let from_cache: Vec<Document> = page_ids
            .iter()
            .filter_map(|id| candidate_doc_map.get(id).map(|d| (*d).clone()))
            .collect();
        let page_docs = if from_cache.len() == page_ids.len() {
            from_cache
        } else {
            self.fetch_docs(&page_ids).await
        };
        let doc_map: HashMap<u64, Document> = page_docs.into_iter().map(|d| (d.id, d)).collect();
        page_hits
            .into_iter()
            .map(|(id, score)| SearchHit { id, score, document: doc_map.get(&id).cloned() })
            .collect()
    }

    /// Route an index request to the correct shard by doc_id.
    /// Also forwards fire-and-forget to a single suggest shard (hash-partitioned).
    pub async fn index(&self, doc: Document) -> search_core::Result<()> {
        metrics::counter!("index_docs_total").increment(1);
        self.fan_out_suggest_write(std::slice::from_ref(&doc));
        let shard_idx = self.shard_for(doc.id);
        self.shards[shard_idx].write_target().index(doc).await
    }

    /// Bulk index: group docs by target shard, send each group in parallel.
    /// Also forwards fire-and-forget to suggest shards (hash-partitioned).
    pub async fn bulk_index(&self, docs: Vec<Document>) -> search_core::Result<u32> {
        self.fan_out_suggest_write(&docs);
        let mut by_shard: Vec<Vec<Document>> = vec![Vec::new(); self.shards.len()];
        for doc in docs {
            by_shard[self.shard_for(doc.id)].push(doc);
        }
        let mut tasks = Vec::new();
        for (shard, batch) in self.shards.iter().zip(by_shard.into_iter()) {
            if batch.is_empty() { continue; }
            let shard = Arc::clone(shard.write_target());
            tasks.push(tokio::spawn(async move { shard.bulk(batch).await }));
        }
        let mut total = 0u32;
        for t in tasks {
            total += t.await.map_err(|e| search_core::Error::Internal(e.to_string()))??;
        }
        metrics::counter!("index_docs_total").increment(total as u64);
        Ok(total)
    }

    /// Forward writes to suggest shards, hash-partitioned by doc_id.
    /// Fire-and-forget: never blocks the caller, never returns errors.
    /// Each doc is cloned once (moved into its target partition), not N times.
    fn fan_out_suggest_write(&self, docs: &[Document]) {
        let n = self.suggest_shards.len();
        if n == 0 || docs.is_empty() {
            return;
        }

        // Partition by doc_id — one clone per doc, regardless of shard count.
        let mut partitions: Vec<Vec<Document>> = (0..n).map(|_| Vec::new()).collect();
        for doc in docs {
            let idx =
                crate::routing::jump_consistent_hash(doc.id, n as u32) as usize;
            partitions[idx].push(doc.clone());
        }

        let timeout = self.suggest_write_timeout;
        for (shard, batch) in self.suggest_shards.iter().zip(partitions.into_iter()) {
            if batch.is_empty() {
                continue;
            }
            let shard = Arc::clone(shard);
            tokio::spawn(async move {
                let fut = shard.bulk(batch);
                match tokio::time::timeout(timeout, fut).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        metrics::counter!("suggest_write_errors_total").increment(1);
                        tracing::debug!("suggest write error (dropped): {e}");
                    }
                    Err(_) => {
                        metrics::counter!("suggest_write_errors_total").increment(1);
                        tracing::debug!("suggest write timed out (dropped)");
                    }
                }
            });
        }
    }

    /// Prefix-based suggest: fans out to all suggest shards, merges by max-score
    /// per term, returns top-`limit`. Returns empty if no suggest shards configured.
    #[tracing::instrument(skip_all, fields(prefix = %prefix, field = %field, limit))]
    pub async fn suggest(
        &self,
        prefix: &str,
        field: &str,
        limit: usize,
    ) -> search_core::Result<Vec<search_suggest::SuggestTerm>> {
        metrics::counter!("suggest_requests_total").increment(1);
        if self.suggest_shards.is_empty() || prefix.is_empty() || limit == 0 {
            return Ok(vec![]);
        }

        let timeout = self.suggest_query_timeout;
        let futures: Vec<_> = self
            .suggest_shards
            .iter()
            .map(|s| {
                let s = Arc::clone(s);
                let prefix = prefix.to_string();
                let field = field.to_string();
                async move {
                    tokio::time::timeout(timeout, s.suggest(&prefix, &field, limit)).await
                }
            })
            .collect();
        let results = futures::future::join_all(futures).await;

        let mut merged: HashMap<String, f32> = HashMap::new();
        for res in results {
            match res {
                Ok(Ok(entries)) => {
                    for e in entries {
                        let slot = merged.entry(e.term).or_insert(0.0);
                        if e.score > *slot {
                            *slot = e.score;
                        }
                    }
                }
                Ok(Err(e)) => tracing::debug!("suggest shard error: {e}"),
                Err(_) => tracing::debug!("suggest shard timeout"),
            }
        }

        let mut out: Vec<search_suggest::SuggestTerm> = merged
            .into_iter()
            .map(|(term, score)| search_suggest::SuggestTerm { term, score })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.term.cmp(&b.term))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Delete a document — route to correct shard by doc_id.
    pub async fn delete(&self, doc_id: u64) -> search_core::Result<()> {
        let shard_idx = self.shard_for(doc_id);
        self.shards[shard_idx].write_target().delete(doc_id).await
    }

    /// Reindex all shards in parallel — rebuilds index from stored docs using current schema.
    pub async fn reindex(&self) -> search_core::Result<Vec<ReindexStats>> {
        let tasks: Vec<_> = self.shards.iter().map(|s| {
            let s = Arc::clone(s.write_target());
            tokio::spawn(async move { s.reindex().await })
        }).collect();
        let mut stats = Vec::new();
        for t in tasks {
            stats.push(t.await.map_err(|e| search_core::Error::Internal(e.to_string()))??);
        }
        Ok(stats)
    }

    /// Flush all shards in parallel.
    pub async fn flush(&self) -> search_core::Result<()> {
        let tasks: Vec<_> = self.shards.iter().map(|s| {
            let s = Arc::clone(s.write_target());
            tokio::spawn(async move { s.flush().await })
        }).collect();
        for t in tasks {
            t.await.map_err(|e| search_core::Error::Internal(e.to_string()))??;
        }
        Ok(())
    }

    pub async fn stats(&self) -> RouterStats {
        let mut shards = Vec::new();
        for group in &self.shards {
            if let Ok(primary) = group.write_target().stats().await {
                let mut replicas = Vec::new();
                for replica in group.replica_clients() {
                    if let Ok(s) = replica.stats().await {
                        replicas.push(s);
                    }
                }
                shards.push(ShardGroupStats { primary, replicas });
            }
        }
        RouterStats { shards }
    }

    fn shard_for(&self, doc_id: u64) -> usize {
        jump_consistent_hash(doc_id, self.shards.len() as u32) as usize
    }

    async fn fetch_docs(&self, doc_ids: &[u64]) -> Vec<Document> {
        if doc_ids.is_empty() {
            return vec![];
        }
        // Group by target shard
        let mut by_shard: Vec<Vec<u64>> = vec![Vec::new(); self.shards.len()];
        for &id in doc_ids {
            let idx = self.shard_for(id);
            by_shard[idx].push(id);
        }

        let futures: Vec<_> = by_shard
            .into_iter()
            .enumerate()
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(idx, ids)| {
                let shard = Arc::clone(self.shards[idx].read_target());
                async move { shard.get_docs(&ids).await.unwrap_or_default() }
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        results.into_iter().flatten().collect()
    }
}
