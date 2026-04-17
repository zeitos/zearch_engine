use crate::filter::{LowercaseFilter, StemmerFilter, TokenFilter};
use crate::token::Token;
use crate::tokenizer::{Tokenizer, UnicodeTokenizer};
use search_core::{AnalyzerType, Language};

/// An analyzer chains a tokenizer with a sequence of token filters.
pub struct Analyzer {
    tokenizer: Box<dyn Tokenizer>,
    filters: Vec<Box<dyn TokenFilter>>,
}

impl Analyzer {
    pub fn new(tokenizer: Box<dyn Tokenizer>, filters: Vec<Box<dyn TokenFilter>>) -> Self {
        Self { tokenizer, filters }
    }

    /// Analyze text: tokenize then apply all filters in order.
    pub fn analyze(&self, text: &str) -> Vec<Token> {
        let mut tokens = self.tokenizer.tokenize(text);
        for filter in &self.filters {
            tokens = filter.apply(tokens);
        }
        tokens
    }

    /// Convenience: analyze and return just the token strings.
    pub fn analyze_to_terms(&self, text: &str) -> Vec<String> {
        self.analyze(text)
            .into_iter()
            .map(|t| t.text)
            .collect()
    }
}

/// Builds an `Analyzer` from an `AnalyzerType` configuration.
pub struct AnalyzerFactory;

impl AnalyzerFactory {
    pub fn build(analyzer_type: &AnalyzerType) -> Analyzer {
        match analyzer_type {
            AnalyzerType::Standard { language } => Self::standard(*language),
            AnalyzerType::Keyword => Self::keyword(),
        }
    }

    /// Standard analyzer: Unicode tokenizer → lowercase → stemmer.
    fn standard(language: Language) -> Analyzer {
        Analyzer::new(
            Box::new(UnicodeTokenizer),
            vec![
                Box::new(LowercaseFilter),
                Box::new(StemmerFilter::new(language)),
            ],
        )
    }

    /// Keyword analyzer: no tokenization, no filtering.
    /// Returns the entire input as a single token.
    fn keyword() -> Analyzer {
        Analyzer::new(Box::new(KeywordTokenizer), vec![])
    }
}

/// Tokenizer that returns the entire input as a single token (for keyword fields).
struct KeywordTokenizer;

impl Tokenizer for KeywordTokenizer {
    fn tokenize(&self, text: &str) -> Vec<Token> {
        if text.is_empty() {
            return vec![];
        }
        vec![Token {
            text: text.to_string(),
            start: 0,
            end: text.len(),
            position: 0,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_english_pipeline() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Standard {
            language: Language::English,
        });
        let terms = analyzer.analyze_to_terms("Samsung Galaxy S24");
        assert_eq!(terms, vec!["samsung", "galaxi", "s24"]);
    }

    #[test]
    fn test_standard_english_plurals() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Standard {
            language: Language::English,
        });
        let singular = analyzer.analyze_to_terms("shoe");
        let plural = analyzer.analyze_to_terms("shoes");
        assert_eq!(singular, plural);
    }

    #[test]
    fn test_standard_spanish_pipeline() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Standard {
            language: Language::Spanish,
        });
        let terms = analyzer.analyze_to_terms("Teléfonos Móviles Samsung");
        // Should lowercase and stem
        assert!(terms.len() == 3);
        assert!(terms[0] != "Teléfonos"); // lowercased + stemmed
    }

    #[test]
    fn test_standard_portuguese_pipeline() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Standard {
            language: Language::Portuguese,
        });
        let terms = analyzer.analyze_to_terms("Computadores Portáteis");
        assert!(terms.len() == 2);
        assert!(terms[0] != "Computadores"); // lowercased + stemmed
    }

    #[test]
    fn test_keyword_analyzer() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Keyword);
        let terms = analyzer.analyze_to_terms("Electronics > Phones > Samsung");
        assert_eq!(terms, vec!["Electronics > Phones > Samsung"]);
    }

    #[test]
    fn test_keyword_analyzer_empty() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Keyword);
        let terms = analyzer.analyze_to_terms("");
        assert!(terms.is_empty());
    }

    #[test]
    fn test_case_insensitive_matching() {
        let analyzer = AnalyzerFactory::build(&AnalyzerType::Standard {
            language: Language::English,
        });
        let upper = analyzer.analyze_to_terms("LAPTOP");
        let lower = analyzer.analyze_to_terms("laptop");
        let mixed = analyzer.analyze_to_terms("Laptop");
        assert_eq!(upper, lower);
        assert_eq!(lower, mixed);
    }
}
