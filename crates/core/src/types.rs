use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A product document to be indexed and searched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Document {
    pub id: u64,
    pub title: String,
    pub description: String,
    pub price: f64,
    pub category: String,
    #[serde(default)]
    pub attributes: HashMap<String, Value>,
}

/// Dynamic attribute value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    StringArray(Vec<String>),
}

// ---------------------------------------------------------------------------
// Search request / response
// ---------------------------------------------------------------------------

/// Search request from the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub filters: HashMap<String, FilterValue>,
    #[serde(default)]
    pub aggregations: Vec<String>,
    #[serde(default)]
    pub sort: Option<SortSpec>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_true")]
    pub typo_tolerance: bool,
    #[serde(default)]
    pub language: Option<String>,
}

fn default_limit() -> usize {
    20
}

fn default_true() -> bool {
    true
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            filters: HashMap::new(),
            aggregations: Vec::new(),
            sort: None,
            offset: 0,
            limit: 20,
            typo_tolerance: true,
            language: None,
        }
    }
}

/// Filter value in a search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterValue {
    Equality { eq: String },
    Range { gte: Option<f64>, lte: Option<f64> },
    MultiValue { r#in: Vec<String> },
}

/// Sort specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortSpec {
    pub field: String,
    #[serde(default)]
    pub order: SortOrder,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    #[default]
    Desc,
    Asc,
}

/// A scored search hit returned to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: u64,
    pub score: f32,
    #[serde(flatten)]
    pub document: Document,
}

/// Aggregation bucket: a value and its document count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregationBucket {
    pub value: String,
    pub count: u64,
}

/// Search response to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub total_hits: u64,
    #[serde(default)]
    pub aggregations: HashMap<String, Vec<AggregationBucket>>,
    pub reranked: bool,
    pub took_ms: u64,
}

// ---------------------------------------------------------------------------
// Ranking types (shared between router and ranker crates)
// ---------------------------------------------------------------------------

/// A candidate document sent to the re-ranker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankCandidate {
    pub doc_id: u64,
    pub bm25_score: f32,
    pub title: String,
    pub category: String,
    pub price: f64,
    #[serde(default)]
    pub attributes: HashMap<String, Value>,
}

/// A re-ranked result returned by the ranker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedResult {
    pub doc_id: u64,
    pub score: f32,
}
