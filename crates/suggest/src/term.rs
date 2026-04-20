use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SuggestTerm {
    pub term: String,
    pub score: f32,
}
