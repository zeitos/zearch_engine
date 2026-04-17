use crate::token::Token;
use unicode_segmentation::UnicodeSegmentation;

/// Trait for splitting text into tokens.
pub trait Tokenizer: Send + Sync {
    fn tokenize(&self, text: &str) -> Vec<Token>;
}

/// Unicode-aware word boundary tokenizer.
///
/// Splits on Unicode word boundaries, keeps only tokens that contain
/// at least one alphanumeric character, and strips leading/trailing
/// non-alphanumeric characters from each token.
pub struct UnicodeTokenizer;

impl Tokenizer for UnicodeTokenizer {
    fn tokenize(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut position = 0;

        for (byte_offset, word) in text.unicode_word_indices() {
            // Skip words that are purely punctuation/whitespace
            if !word.chars().any(|c| c.is_alphanumeric()) {
                continue;
            }

            // Trim non-alphanumeric from start and end
            let trimmed_start = word
                .char_indices()
                .find(|(_, c)| c.is_alphanumeric())
                .map(|(i, _)| i)
                .unwrap_or(0);
            let trimmed_end = word
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_alphanumeric())
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(word.len());

            let trimmed = &word[trimmed_start..trimmed_end];
            if trimmed.is_empty() {
                continue;
            }

            tokens.push(Token {
                text: trimmed.to_string(),
                start: byte_offset + trimmed_start,
                end: byte_offset + trimmed_end,
                position,
            });
            position += 1;
        }

        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokenization() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("hello world");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "hello");
        assert_eq!(tokens[1].text, "world");
        assert_eq!(tokens[0].position, 0);
        assert_eq!(tokens[1].position, 1);
    }

    #[test]
    fn test_punctuation() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("hello, world! How's it going?");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"hello"));
        assert!(texts.contains(&"world"));
        assert!(texts.contains(&"How's"));
        assert!(texts.contains(&"going"));
    }

    #[test]
    fn test_numbers() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("Samsung Galaxy S24 256GB $799.99");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"Samsung"));
        assert!(texts.contains(&"Galaxy"));
        assert!(texts.contains(&"S24"));
        assert!(texts.contains(&"256GB"));
        // Price may split depending on word boundaries
    }

    #[test]
    fn test_unicode_spanish() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("teléfono móvil Samsung");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"teléfono"));
        assert!(texts.contains(&"móvil"));
        assert!(texts.contains(&"Samsung"));
    }

    #[test]
    fn test_unicode_portuguese() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("computação gráfica avançada");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"computação"));
        assert!(texts.contains(&"gráfica"));
        assert!(texts.contains(&"avançada"));
    }

    #[test]
    fn test_empty_input() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_only_punctuation() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("!!! --- ???");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_positions_are_sequential() {
        let tokenizer = UnicodeTokenizer;
        let tokens = tokenizer.tokenize("one two three four five");
        for (i, token) in tokens.iter().enumerate() {
            assert_eq!(token.position, i);
        }
    }
}
