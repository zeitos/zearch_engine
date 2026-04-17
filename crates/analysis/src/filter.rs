use crate::token::Token;
use rust_stemmers::{Algorithm, Stemmer};
use search_core::Language;

/// Trait for transforming a stream of tokens.
pub trait TokenFilter: Send + Sync {
    fn apply(&self, tokens: Vec<Token>) -> Vec<Token>;
}

/// Converts all token text to lowercase (Unicode-aware).
pub struct LowercaseFilter;

impl TokenFilter for LowercaseFilter {
    fn apply(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut t| {
                t.text = t.text.to_lowercase();
                t
            })
            .collect()
    }
}

/// Applies Snowball stemming for a given language.
pub struct StemmerFilter {
    stemmer: Stemmer,
}

impl StemmerFilter {
    pub fn new(language: Language) -> Self {
        let algorithm = match language {
            Language::English => Algorithm::English,
            Language::Spanish => Algorithm::Spanish,
            Language::Portuguese => Algorithm::Portuguese,
        };
        Self {
            stemmer: Stemmer::create(algorithm),
        }
    }
}

impl TokenFilter for StemmerFilter {
    fn apply(&self, tokens: Vec<Token>) -> Vec<Token> {
        tokens
            .into_iter()
            .map(|mut t| {
                let stemmed = self.stemmer.stem(&t.text);
                t.text = stemmed.into_owned();
                t
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lowercase_basic() {
        let filter = LowercaseFilter;
        let tokens = vec![
            Token { text: "Hello".into(), start: 0, end: 5, position: 0 },
            Token { text: "WORLD".into(), start: 6, end: 11, position: 1 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, "hello");
        assert_eq!(result[1].text, "world");
    }

    #[test]
    fn test_lowercase_unicode() {
        let filter = LowercaseFilter;
        let tokens = vec![
            Token { text: "TELÉFONO".into(), start: 0, end: 9, position: 0 },
            Token { text: "MÓVIL".into(), start: 10, end: 16, position: 1 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, "teléfono");
        assert_eq!(result[1].text, "móvil");
    }

    // -- English stemming --

    #[test]
    fn test_stemmer_english_plurals() {
        let filter = StemmerFilter::new(Language::English);
        let tokens = vec![
            Token { text: "shoes".into(), start: 0, end: 5, position: 0 },
            Token { text: "shoe".into(), start: 6, end: 10, position: 1 },
            Token { text: "running".into(), start: 11, end: 18, position: 2 },
            Token { text: "runs".into(), start: 19, end: 23, position: 3 },
        ];
        let result = filter.apply(tokens);
        // "shoes" and "shoe" should stem to the same thing
        assert_eq!(result[0].text, result[1].text);
        // "running" and "runs" should stem to the same thing
        assert_eq!(result[2].text, result[3].text);
    }

    #[test]
    fn test_stemmer_english_ies() {
        let filter = StemmerFilter::new(Language::English);
        let tokens = vec![
            Token { text: "batteries".into(), start: 0, end: 9, position: 0 },
            Token { text: "battery".into(), start: 10, end: 17, position: 1 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, result[1].text);
    }

    // -- Spanish stemming --

    #[test]
    fn test_stemmer_spanish_plurals() {
        let filter = StemmerFilter::new(Language::Spanish);
        let tokens = vec![
            Token { text: "zapatos".into(), start: 0, end: 7, position: 0 },
            Token { text: "zapato".into(), start: 8, end: 14, position: 1 },
            Token { text: "computadoras".into(), start: 15, end: 27, position: 2 },
            Token { text: "computadora".into(), start: 28, end: 39, position: 3 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, result[1].text);
        assert_eq!(result[2].text, result[3].text);
    }

    #[test]
    fn test_stemmer_spanish_verbs() {
        let filter = StemmerFilter::new(Language::Spanish);
        let tokens = vec![
            Token { text: "corriendo".into(), start: 0, end: 9, position: 0 },
            Token { text: "correr".into(), start: 10, end: 16, position: 1 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, result[1].text);
    }

    // -- Portuguese stemming --

    #[test]
    fn test_stemmer_portuguese_plurals() {
        let filter = StemmerFilter::new(Language::Portuguese);
        let tokens = vec![
            Token { text: "sapatos".into(), start: 0, end: 7, position: 0 },
            Token { text: "sapato".into(), start: 8, end: 14, position: 1 },
            Token { text: "computadores".into(), start: 15, end: 27, position: 2 },
            Token { text: "computador".into(), start: 28, end: 38, position: 3 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, result[1].text);
        assert_eq!(result[2].text, result[3].text);
    }

    #[test]
    fn test_stemmer_portuguese_verbs() {
        let filter = StemmerFilter::new(Language::Portuguese);
        let tokens = vec![
            Token { text: "correndo".into(), start: 0, end: 8, position: 0 },
            Token { text: "correr".into(), start: 9, end: 15, position: 1 },
        ];
        let result = filter.apply(tokens);
        assert_eq!(result[0].text, result[1].text);
    }
}
