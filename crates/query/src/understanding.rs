use search_analysis::AnalyzerFactory;
use search_core::{AnalyzerType, FieldType, IndexSchema, Language};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BooleanMode {
    And,
    OrFallback,
}

/// Structured representation of a query after the QU pipeline.
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    pub original: String,
    pub tokens: Vec<String>,
    pub language: Language,
    pub boolean_mode: BooleanMode,
    /// Debug log of transformations applied by analyzers.
    pub transformations: Vec<String>,
}

impl ParsedQuery {
    pub fn new(raw: &str) -> Self {
        Self {
            original: raw.to_string(),
            tokens: vec![],
            language: Language::Spanish,
            boolean_mode: BooleanMode::And,
            transformations: vec![],
        }
    }
}

/// A single stage in the QU pipeline.
pub trait QueryAnalyzer: Send + Sync {
    fn analyze(&self, query: ParsedQuery) -> ParsedQuery;
}

// ---------------------------------------------------------------------------
// Built-in analyzers
// ---------------------------------------------------------------------------

/// Tokenizes the raw query using the schema's primary text analyzer.
pub struct TokenizerAnalyzer {
    analyzer_type: AnalyzerType,
}

impl TokenizerAnalyzer {
    pub fn new(schema: &IndexSchema) -> Self {
        let analyzer_type = schema
            .fields
            .iter()
            .find(|f| f.indexed && f.field_type == FieldType::Text)
            .map(|f| f.analyzer.clone())
            .unwrap_or(AnalyzerType::Keyword);
        Self { analyzer_type }
    }
}

impl QueryAnalyzer for TokenizerAnalyzer {
    fn analyze(&self, mut query: ParsedQuery) -> ParsedQuery {
        let analyzer = AnalyzerFactory::build(&self.analyzer_type);
        let token_structs = analyzer.analyze(&query.original);
        query.tokens = token_structs.into_iter().map(|t| t.text).collect();
        query.transformations.push(format!("tokenized → {} tokens", query.tokens.len()));
        query
    }
}

/// Detects or inherits language from the request, sets it on the ParsedQuery.
pub struct LanguageAnalyzer {
    override_language: Option<Language>,
}

impl LanguageAnalyzer {
    pub fn new(override_language: Option<Language>) -> Self {
        Self { override_language }
    }
}

impl Default for LanguageAnalyzer {
    fn default() -> Self {
        Self { override_language: None }
    }
}

impl QueryAnalyzer for LanguageAnalyzer {
    fn analyze(&self, mut query: ParsedQuery) -> ParsedQuery {
        if let Some(lang) = self.override_language {
            query.language = lang;
            query.transformations.push(format!("language override → {lang:?}"));
        }
        query
    }
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

pub struct QUPipeline {
    analyzers: Vec<Box<dyn QueryAnalyzer>>,
}

impl QUPipeline {
    pub fn new(analyzers: Vec<Box<dyn QueryAnalyzer>>) -> Self {
        Self { analyzers }
    }

    pub fn default_for_schema(schema: &IndexSchema) -> Self {
        Self::new(vec![
            Box::new(TokenizerAnalyzer::new(schema)),
            Box::new(LanguageAnalyzer::default()),
        ])
    }

    pub fn run(&self, raw: &str) -> ParsedQuery {
        let mut pq = ParsedQuery::new(raw);
        for analyzer in &self.analyzers {
            pq = analyzer.analyze(pq);
        }
        tracing::debug!(
            query = raw,
            tokens = ?pq.tokens,
            transformations = ?pq.transformations,
            "QU pipeline complete"
        );
        pq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::IndexSchema;

    #[test]
    fn test_tokenizer_analyzer() {
        let schema = IndexSchema::default_product_schema();
        let pipeline = QUPipeline::default_for_schema(&schema);
        let pq = pipeline.run("Computadora gamer i7");
        assert!(!pq.tokens.is_empty());
        assert_eq!(pq.boolean_mode, BooleanMode::And);
    }

    #[test]
    fn test_empty_query_produces_no_tokens() {
        let schema = IndexSchema::default_product_schema();
        let pipeline = QUPipeline::default_for_schema(&schema);
        let pq = pipeline.run("");
        assert!(pq.tokens.is_empty());
    }

    #[test]
    fn test_language_override() {
        use search_core::Language;
        let schema = IndexSchema::default_product_schema();
        let pipeline = QUPipeline::new(vec![
            Box::new(TokenizerAnalyzer::new(&schema)),
            Box::new(LanguageAnalyzer::new(Some(Language::Portuguese))),
        ]);
        let pq = pipeline.run("computador");
        assert_eq!(pq.language, Language::Portuguese);
    }
}
