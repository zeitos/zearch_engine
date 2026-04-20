use async_trait::async_trait;
use search_core::{
    AvailabilityConfig, AvailabilityStrategyType, Document, RegionStockConfig,
    StaticAvailabilityConfig, Value,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[async_trait]
pub trait AvailabilityStrategy: Send + Sync {
    /// Remove unavailable candidates in-place.
    /// Only called when `zone` is non-empty.
    async fn apply(
        &self,
        candidates: &mut Vec<(u64, f32)>,
        docs: &HashMap<u64, &Document>,
        zone: &str,
    );

    /// Multiply scatter fetch size to compensate for expected filtering loss.
    fn candidate_multiplier(&self) -> usize {
        1
    }
}

// ---------------------------------------------------------------------------
// NoopStrategy
// ---------------------------------------------------------------------------

pub struct NoopStrategy;

#[async_trait]
impl AvailabilityStrategy for NoopStrategy {
    async fn apply(
        &self,
        _candidates: &mut Vec<(u64, f32)>,
        _docs: &HashMap<u64, &Document>,
        _zone: &str,
    ) {
    }
}

// ---------------------------------------------------------------------------
// StaticAvailabilityStrategy
// ---------------------------------------------------------------------------

pub struct StaticAvailabilityStrategy {
    stock_attribute_prefix: String,
    default_available: bool,
    min_stock: f64,
}

impl StaticAvailabilityStrategy {
    pub fn new(cfg: StaticAvailabilityConfig) -> Self {
        Self {
            stock_attribute_prefix: cfg.stock_attribute_prefix,
            default_available: cfg.default_available,
            min_stock: cfg.min_stock,
        }
    }
}

#[async_trait]
impl AvailabilityStrategy for StaticAvailabilityStrategy {
    async fn apply(
        &self,
        candidates: &mut Vec<(u64, f32)>,
        docs: &HashMap<u64, &Document>,
        zone: &str,
    ) {
        let attr_key = format!("{}{}", self.stock_attribute_prefix, zone);
        let default_available = self.default_available;
        let min_stock = self.min_stock;

        let before = candidates.len();
        candidates.retain(|(id, _)| {
            let Some(doc) = docs.get(id) else {
                return default_available;
            };
            match doc.attributes.get(&attr_key) {
                None => default_available,
                Some(Value::Number(n)) => *n >= min_stock,
                Some(Value::String(s)) => {
                    s.parse::<f64>().map_or(default_available, |n| n >= min_stock)
                }
                _ => default_available,
            }
        });

        let removed = before.saturating_sub(candidates.len());
        if removed > 0 {
            metrics::counter!("availability_filter_removed_total").increment(removed as u64);
        }
    }

    fn candidate_multiplier(&self) -> usize {
        3
    }
}

// ---------------------------------------------------------------------------
// RegionStockStrategy
// ---------------------------------------------------------------------------

pub struct RegionStockStrategy {
    client: reqwest::Client,
    base_url: String,
    timeout: Duration,
}

impl RegionStockStrategy {
    pub fn new(cfg: RegionStockConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: cfg.base_url,
            timeout: Duration::from_millis(cfg.timeout_ms),
        }
    }
}

#[derive(serde::Deserialize)]
struct StockResponse {
    available_ids: Vec<u64>,
}

#[async_trait]
impl AvailabilityStrategy for RegionStockStrategy {
    async fn apply(
        &self,
        candidates: &mut Vec<(u64, f32)>,
        _docs: &HashMap<u64, &Document>,
        zone: &str,
    ) {
        if candidates.is_empty() {
            return;
        }

        let ids: Vec<String> = candidates.iter().map(|(id, _)| id.to_string()).collect();
        let url = format!("{}/availability", self.base_url);

        let result = self
            .client
            .get(&url)
            .query(&[("zone", zone), ("doc_ids", &ids.join(","))])
            .timeout(self.timeout)
            .send()
            .await;

        match result {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<StockResponse>().await {
                    Ok(body) => {
                        let available: std::collections::HashSet<u64> =
                            body.available_ids.into_iter().collect();
                        let before = candidates.len();
                        candidates.retain(|(id, _)| available.contains(id));
                        let removed = before.saturating_sub(candidates.len());
                        if removed > 0 {
                            metrics::counter!("availability_filter_removed_total")
                                .increment(removed as u64);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("availability: failed to parse stock response: {e}");
                        metrics::counter!("availability_stock_service_errors_total").increment(1);
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!("availability: stock service returned {}", resp.status());
                metrics::counter!("availability_stock_service_errors_total").increment(1);
            }
            Err(e) => {
                tracing::warn!("availability: stock service error: {e}");
                metrics::counter!("availability_stock_service_errors_total").increment(1);
            }
        }
    }

    fn candidate_multiplier(&self) -> usize {
        3
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub fn build_strategy(config: &AvailabilityConfig) -> Arc<dyn AvailabilityStrategy> {
    match config.strategy {
        AvailabilityStrategyType::Noop => Arc::new(NoopStrategy),
        AvailabilityStrategyType::Static => {
            let cfg = config.r#static.clone().unwrap_or_default();
            Arc::new(StaticAvailabilityStrategy::new(cfg))
        }
        AvailabilityStrategyType::RegionStock => {
            let cfg = config
                .region_stock
                .clone()
                .expect("region_stock config required when strategy = region_stock");
            Arc::new(RegionStockStrategy::new(cfg))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{StaticAvailabilityConfig, Value};

    fn doc_with_stock(id: u64, zone: &str, stock: f64) -> Document {
        let mut attrs = HashMap::new();
        attrs.insert(format!("stock_{zone}"), Value::Number(stock));
        Document {
            id,
            title: "test".into(),
            description: "".into(),
            price: 0.0,
            category: "x".into(),
            attributes: attrs,
        }
    }

    fn no_stock_doc(id: u64) -> Document {
        Document {
            id,
            title: "test".into(),
            description: "".into(),
            price: 0.0,
            category: "x".into(),
            attributes: HashMap::new(),
        }
    }

    fn doc_map(docs: &[Document]) -> HashMap<u64, &Document> {
        docs.iter().map(|d| (d.id, d)).collect()
    }

    #[tokio::test]
    async fn test_noop_passes_all() {
        let strat = NoopStrategy;
        let mut candidates = vec![(1u64, 1.0f32), (2, 0.5)];
        let docs: Vec<Document> = vec![];
        strat.apply(&mut candidates, &doc_map(&docs), "buenos_aires").await;
        assert_eq!(candidates.len(), 2);
    }

    #[tokio::test]
    async fn test_static_filters_zero_stock() {
        let strat = StaticAvailabilityStrategy::new(StaticAvailabilityConfig::default());
        let docs = vec![
            doc_with_stock(1, "buenos_aires", 5.0),
            doc_with_stock(2, "buenos_aires", 0.0),
            doc_with_stock(3, "buenos_aires", 3.0),
        ];
        let mut candidates = vec![(1u64, 1.0f32), (2, 0.8), (3, 0.6)];
        strat.apply(&mut candidates, &doc_map(&docs), "buenos_aires").await;
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|(id, _)| *id != 2));
    }

    #[tokio::test]
    async fn test_static_default_available_true() {
        let strat = StaticAvailabilityStrategy::new(StaticAvailabilityConfig {
            default_available: true,
            ..Default::default()
        });
        let docs = vec![no_stock_doc(1)];
        let mut candidates = vec![(1u64, 1.0f32)];
        strat.apply(&mut candidates, &doc_map(&docs), "santa_cruz").await;
        assert_eq!(candidates.len(), 1, "missing attribute with default_available=true should pass");
    }

    #[tokio::test]
    async fn test_static_default_available_false() {
        let strat = StaticAvailabilityStrategy::new(StaticAvailabilityConfig {
            default_available: false,
            ..Default::default()
        });
        let docs = vec![no_stock_doc(1)];
        let mut candidates = vec![(1u64, 1.0f32)];
        strat.apply(&mut candidates, &doc_map(&docs), "santa_cruz").await;
        assert_eq!(candidates.len(), 0, "missing attribute with default_available=false should be removed");
    }

    #[tokio::test]
    async fn test_region_stock_fail_open() {
        // Points at a non-existent server — should fail open, keeping all candidates
        let strat = RegionStockStrategy::new(RegionStockConfig {
            base_url: "http://127.0.0.1:19999".into(),
            timeout_ms: 50,
        });
        let docs = vec![no_stock_doc(1), no_stock_doc(2)];
        let mut candidates = vec![(1u64, 1.0f32), (2, 0.5)];
        strat.apply(&mut candidates, &doc_map(&docs), "buenos_aires").await;
        assert_eq!(candidates.len(), 2, "fail-open: all candidates should survive a stock service error");
    }
}
