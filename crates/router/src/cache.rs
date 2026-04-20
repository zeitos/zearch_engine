use moka::future::Cache;
use search_core::{AggregationBucket, FilterValue, QueryCacheConfig, RetrievalMode, SearchRequest, SortOrder};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Cache key — deterministic hash regardless of HashMap insertion order
// ---------------------------------------------------------------------------

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct QueryCacheKey {
    query: String,
    /// Sorted (field, serialized_value) pairs for deterministic hashing.
    filters: Vec<(String, String)>,
    aggregations: Vec<String>,
    sort: Option<(String, bool)>, // (field, is_asc)
    offset: usize,
    limit: usize,
    typo_tolerance: bool,
    language: Option<String>,
    destination_zone: Option<String>,
}

impl QueryCacheKey {
    pub fn from_request(req: &SearchRequest) -> Self {
        let mut filters: Vec<(String, String)> = req
            .filters
            .iter()
            .map(|(k, v)| {
                let val = match v {
                    FilterValue::Equality { eq } => format!("eq:{eq}"),
                    FilterValue::Range { gte, lte } => format!("range:{gte:?}:{lte:?}"),
                    FilterValue::MultiValue { r#in } => {
                        let mut vals = r#in.clone();
                        vals.sort();
                        format!("in:{}", vals.join(","))
                    }
                };
                (k.clone(), val)
            })
            .collect();
        filters.sort_by(|a, b| a.0.cmp(&b.0));

        Self {
            query: req.query.clone(),
            filters,
            aggregations: req.aggregations.clone(),
            sort: req.sort.as_ref().map(|s| (s.field.clone(), s.order == SortOrder::Asc)),
            offset: req.offset,
            limit: req.limit,
            typo_tolerance: req.typo_tolerance,
            language: req.language.clone(),
            destination_zone: req.destination_zone.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Cached value
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CachedResponse {
    pub hits: Vec<(u64, f32)>,
    pub total_hits: u64,
    pub aggregations: HashMap<String, Vec<AggregationBucket>>,
    pub reranked: bool,
    pub retrieval_mode: RetrievalMode,
}

// ---------------------------------------------------------------------------
// QueryCache wrapper
// ---------------------------------------------------------------------------

pub struct QueryCache {
    inner: Cache<QueryCacheKey, Arc<CachedResponse>>,
}

impl QueryCache {
    pub fn build(config: &QueryCacheConfig) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        let cache = Cache::builder()
            .max_capacity(config.max_entries)
            .time_to_live(Duration::from_secs(config.ttl_seconds))
            .build();
        Some(Self { inner: cache })
    }

    pub async fn get(&self, key: &QueryCacheKey) -> Option<Arc<CachedResponse>> {
        let result = self.inner.get(key).await;
        if result.is_some() {
            metrics::counter!("search_cache_hits_total").increment(1);
        } else {
            metrics::counter!("search_cache_misses_total").increment(1);
        }
        metrics::gauge!("search_cache_size").set(self.inner.entry_count() as f64);
        result
    }

    pub async fn insert(&self, key: QueryCacheKey, value: CachedResponse) {
        self.inner.insert(key, Arc::new(value)).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{QueryCacheConfig, SearchRequest};

    fn make_key(query: &str) -> QueryCacheKey {
        QueryCacheKey::from_request(&SearchRequest {
            query: query.into(),
            ..Default::default()
        })
    }

    #[test]
    fn test_include_docs_not_in_key() {
        let req_a = SearchRequest { query: "test".into(), include_docs: false, ..Default::default() };
        let req_b = SearchRequest { query: "test".into(), include_docs: true, ..Default::default() };
        assert_eq!(QueryCacheKey::from_request(&req_a), QueryCacheKey::from_request(&req_b));
    }

    #[test]
    fn test_different_zone_different_key() {
        let req_a = SearchRequest { query: "test".into(), destination_zone: Some("zone_a".into()), ..Default::default() };
        let req_b = SearchRequest { query: "test".into(), destination_zone: Some("zone_b".into()), ..Default::default() };
        assert_ne!(QueryCacheKey::from_request(&req_a), QueryCacheKey::from_request(&req_b));
    }

    #[test]
    fn test_filter_order_independent() {
        use search_core::FilterValue;
        let mut filters_a = HashMap::new();
        filters_a.insert("category".into(), FilterValue::Equality { eq: "phones".into() });
        filters_a.insert("brand".into(), FilterValue::Equality { eq: "sony".into() });

        let mut filters_b = HashMap::new();
        filters_b.insert("brand".into(), FilterValue::Equality { eq: "sony".into() });
        filters_b.insert("category".into(), FilterValue::Equality { eq: "phones".into() });

        let key_a = QueryCacheKey::from_request(&SearchRequest { query: "x".into(), filters: filters_a, ..Default::default() });
        let key_b = QueryCacheKey::from_request(&SearchRequest { query: "x".into(), filters: filters_b, ..Default::default() });
        assert_eq!(key_a, key_b);
    }

    #[tokio::test]
    async fn test_cache_hit_and_miss() {
        let config = QueryCacheConfig { enabled: true, max_entries: 100, ttl_seconds: 60 };
        let cache = QueryCache::build(&config).unwrap();

        let key = make_key("samsung");
        assert!(cache.get(&key).await.is_none());

        cache.insert(key.clone(), CachedResponse {
            hits: vec![(1, 0.9), (2, 0.5)],
            total_hits: 2,
            aggregations: HashMap::new(),
            reranked: false,
            retrieval_mode: RetrievalMode::And,
        }).await;

        let hit = cache.get(&key).await.unwrap();
        assert_eq!(hit.hits.len(), 2);
        assert_eq!(hit.hits[0], (1, 0.9));
    }

    #[test]
    fn test_cache_disabled_returns_none() {
        let config = QueryCacheConfig { enabled: false, ..Default::default() };
        assert!(QueryCache::build(&config).is_none());
    }
}
