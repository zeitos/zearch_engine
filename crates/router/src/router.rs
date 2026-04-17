use crate::client::{ShardClient, ShardSearchResult};
use crate::routing::jump_consistent_hash;
use search_core::{
    AggregationBucket, Document, RankCandidate, SearchRequest, SearchResponse, SearchHit,
};
use search_ranker::{rerank_with_fallback, Ranker};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub struct RouterStats {
    pub shards: Vec<search_shard::ShardStats>,
}

pub struct Router {
    shards: Vec<Arc<dyn ShardClient>>,
    ranker: Arc<dyn Ranker>,
    ranker_timeout: Duration,
    /// Number of candidates to send to ranker (top-N before pagination).
    ranker_candidates: usize,
    query_timeout: Duration,
}

impl Router {
    pub fn new(
        shards: Vec<Arc<dyn ShardClient>>,
        ranker: Arc<dyn Ranker>,
        ranker_timeout_ms: u64,
        ranker_candidates: usize,
        query_timeout_ms: u64,
    ) -> Self {
        Self {
            shards,
            ranker,
            ranker_timeout: Duration::from_millis(ranker_timeout_ms),
            ranker_candidates,
            query_timeout: Duration::from_millis(query_timeout_ms),
        }
    }

    /// Fan-out search to all shards, merge, optionally rerank, paginate, fetch docs.
    pub async fn search(&self, request: SearchRequest) -> search_core::Result<SearchResponse> {
        let start = std::time::Instant::now();
        metrics::counter!("search_queries_total").increment(1);

        // 1. Scatter — request candidates beyond the final page to enable cross-shard ranking
        let mut shard_req = request.clone();
        shard_req.offset = 0;
        shard_req.limit = self.ranker_candidates;

        let futures: Vec<_> = self
            .shards
            .iter()
            .map(|shard| {
                let req = shard_req.clone();
                let shard = Arc::clone(shard);
                async move { shard.search(req).await }
            })
            .collect();

        let shard_results: Vec<search_core::Result<ShardSearchResult>> =
            futures::future::join_all(futures).await;

        // 2. Gather — merge scored hits across shards
        let mut all_hits: Vec<(u64, f32)> = Vec::new();
        let mut total_hits: u64 = 0;
        let mut merged_aggs: HashMap<String, HashMap<String, u64>> = HashMap::new();
        let mut healthy_shards = 0usize;

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
                    healthy_shards += 1;
                }
                Err(e) => {
                    tracing::warn!("Shard search failed: {e}");
                }
            }
        }

        if healthy_shards == 0 {
            return Err(search_core::Error::Shard("all shards failed".into()));
        }

        // 3. Sort globally by score descending
        all_hits.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // 4. Optional re-ranking on top-N candidates
        let (final_hits, reranked) = if all_hits.is_empty() {
            (all_hits, false)
        } else {
            // We need doc metadata for ranking; fetch docs for candidates
            let candidate_ids: Vec<u64> = all_hits.iter().map(|(id, _)| *id).collect();
            let candidate_docs = self.fetch_docs(&candidate_ids).await;
            let doc_map: HashMap<u64, &Document> = candidate_docs.iter().map(|d| (d.id, d)).collect();

            let candidates: Vec<RankCandidate> = all_hits
                .iter()
                .filter_map(|(id, score)| {
                    let doc = doc_map.get(id)?;
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
                rerank_with_fallback(self.ranker.as_ref(), &request.query, &candidates, self.ranker_timeout).await;

            let reranked_hits: Vec<(u64, f32)> =
                ranked.into_iter().map(|r| (r.doc_id, r.score)).collect();
            (reranked_hits, did_rerank)
        };

        // 5. Paginate
        let page_hits: Vec<(u64, f32)> = final_hits
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect();

        // 6. Fetch full docs for the result page
        let page_ids: Vec<u64> = page_hits.iter().map(|(id, _)| *id).collect();
        let page_docs = self.fetch_docs(&page_ids).await;
        let doc_map: HashMap<u64, Document> = page_docs.into_iter().map(|d| (d.id, d)).collect();

        let hits: Vec<SearchHit> = page_hits
            .into_iter()
            .filter_map(|(id, score)| {
                let doc = doc_map.get(&id)?.clone();
                Some(SearchHit { id, score, document: doc })
            })
            .collect();

        let aggregations = merged_aggs
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

        let elapsed = start.elapsed();
        metrics::histogram!("search_query_duration_seconds").record(elapsed.as_secs_f64());
        if reranked {
            metrics::counter!("ranker_rerank_total").increment(1);
        }

        Ok(SearchResponse {
            hits,
            total_hits,
            aggregations,
            reranked,
            took_ms: elapsed.as_millis() as u64,
        })
    }

    /// Route an index request to the correct shard by doc_id.
    pub async fn index(&self, doc: Document) -> search_core::Result<()> {
        metrics::counter!("index_docs_total").increment(1);
        let shard_idx = self.shard_for(doc.id);
        self.shards[shard_idx].index(doc).await
    }

    /// Delete a document — try all shards (we don't know which has it).
    pub async fn delete(&self, doc_id: u64) -> search_core::Result<()> {
        let shard_idx = self.shard_for(doc_id);
        self.shards[shard_idx].delete(doc_id).await
    }

    pub async fn stats(&self) -> RouterStats {
        let mut shards = Vec::new();
        for shard in &self.shards {
            if let Ok(s) = shard.stats().await {
                shards.push(s);
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
                let shard = Arc::clone(&self.shards[idx]);
                async move { shard.get_docs(&ids).await.unwrap_or_default() }
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        results.into_iter().flatten().collect()
    }
}
