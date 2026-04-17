/// A single token produced by the analysis pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token text (may be transformed by filters).
    pub text: String,
    /// Start byte offset in the original text.
    pub start: usize,
    /// End byte offset in the original text.
    pub end: usize,
    /// Position in the token stream (0-based).
    pub position: usize,
}
