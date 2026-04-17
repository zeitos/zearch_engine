use serde::{Deserialize, Serialize};

/// Schema definition for an index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSchema {
    pub fields: Vec<FieldConfig>,
}

impl IndexSchema {
    /// Returns the default product schema.
    pub fn default_product_schema() -> Self {
        Self {
            fields: vec![
                FieldConfig {
                    name: "title".into(),
                    field_type: FieldType::Text,
                    indexed: true,
                    stored: true,
                    filterable: false,
                    aggregatable: false,
                    boost: 2.0,
                    analyzer: AnalyzerType::Standard {
                        language: Language::English,
                    },
                },
                FieldConfig {
                    name: "description".into(),
                    field_type: FieldType::Text,
                    indexed: true,
                    stored: true,
                    filterable: false,
                    aggregatable: false,
                    boost: 1.0,
                    analyzer: AnalyzerType::Standard {
                        language: Language::English,
                    },
                },
                FieldConfig {
                    name: "price".into(),
                    field_type: FieldType::Numeric,
                    indexed: false,
                    stored: true,
                    filterable: true,
                    aggregatable: false,
                    boost: 0.0,
                    analyzer: AnalyzerType::Keyword,
                },
                FieldConfig {
                    name: "category".into(),
                    field_type: FieldType::Keyword,
                    indexed: false,
                    stored: true,
                    filterable: true,
                    aggregatable: true,
                    boost: 0.0,
                    analyzer: AnalyzerType::Keyword,
                },
            ],
        }
    }
}

/// Configuration for a single field in the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldConfig {
    pub name: String,
    pub field_type: FieldType,
    /// Whether this field is included in the inverted index for full-text search.
    pub indexed: bool,
    /// Whether this field's value is stored for retrieval.
    pub stored: bool,
    /// Whether this field is stored in the column store for filtering.
    pub filterable: bool,
    /// Whether this field supports count aggregations.
    pub aggregatable: bool,
    /// Relevance weight for this field (default 1.0).
    pub boost: f32,
    /// Text analysis pipeline for this field.
    pub analyzer: AnalyzerType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    /// Analyzed, tokenized text field.
    Text,
    /// Exact match only, not tokenized.
    Keyword,
    /// f64 numeric field for range filters and sorting.
    Numeric,
    /// Boolean field.
    Boolean,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnalyzerType {
    Standard { language: Language },
    Keyword,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    English,
    Spanish,
    Portuguese,
}
