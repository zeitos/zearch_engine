use search_analysis::{Analyzer, AnalyzerFactory};
use search_core::{FieldType, IndexSchema};
use search_index::SegmentReader;
use std::collections::HashMap;

/// Per-segment statistics for BM25 scoring.
pub struct SegmentStatistics {
    pub total_docs: u32,
    /// Average token count per field (for BM25 length normalization).
    pub field_avg_lengths: HashMap<String, f32>,
}

impl SegmentStatistics {
    /// Compute statistics by iterating all live docs in a segment.
    pub fn compute(reader: &SegmentReader, schema: &IndexSchema) -> Self {
        let total_docs = reader.meta.doc_count;
        let mut field_total_lengths: HashMap<String, u64> = HashMap::new();

        let text_fields: Vec<(&str, Analyzer)> = schema
            .fields
            .iter()
            .filter(|f| f.indexed && f.field_type == FieldType::Text)
            .map(|f| (f.name.as_str(), AnalyzerFactory::build(&f.analyzer)))
            .collect();

        for local_id in 0..total_docs {
            let Some(doc) = reader.get_doc(local_id) else { continue };
            for (field_name, analyzer) in &text_fields {
                let text = match *field_name {
                    "title" => &doc.title,
                    "description" => &doc.description,
                    _ => continue,
                };
                let count = analyzer.analyze(text).len() as u64;
                *field_total_lengths.entry(field_name.to_string()).or_default() += count;
            }
        }

        let live = reader.all_live_docs().len() as u32;
        let denom = live.max(1) as f32;
        let field_avg_lengths = field_total_lengths
            .into_iter()
            .map(|(k, v)| (k, v as f32 / denom))
            .collect();

        Self {
            total_docs: live,
            field_avg_lengths,
        }
    }
}

pub const K1: f32 = 1.2;

/// BM25 scorer with configurable k1 and b parameters.
#[derive(Clone)]
pub struct Bm25Scorer {
    pub k1: f32,
    pub b: f32,
}

impl Default for Bm25Scorer {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

impl Bm25Scorer {
    pub fn new(k1: f32, b: f32) -> Self {
        Self { k1, b }
    }

    /// IDF: ln(1 + (N - df + 0.5) / (df + 0.5))
    pub fn idf(&self, total_docs: u32, doc_freq: u32) -> f32 {
        let n = total_docs as f32;
        let df = doc_freq as f32;
        ((1.0 + (n - df + 0.5) / (df + 0.5)).ln()).max(0.0)
    }

    /// BM25 term score for one (term, doc) pair.
    /// tf = term freq in doc, dl = doc length in tokens, avgdl = average doc length
    pub fn term_score(&self, tf: u32, dl: u32, avgdl: f32, idf: f32) -> f32 {
        let tf = tf as f32;
        let dl = dl as f32;
        let avgdl = avgdl.max(1.0);
        let num = tf * (self.k1 + 1.0);
        let denom = tf + self.k1 * (1.0 - self.b + self.b * dl / avgdl);
        idf * num / denom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scorer() -> Bm25Scorer {
        Bm25Scorer::default()
    }

    #[test]
    fn test_idf_rare_term() {
        let s = scorer();
        // 1 doc out of 10000 → high IDF
        let idf = s.idf(10000, 1);
        assert!(idf > 5.0, "idf={idf}");
    }

    #[test]
    fn test_idf_common_term() {
        let s = scorer();
        // term in all docs → IDF near 0
        let idf = s.idf(100, 100);
        assert!(idf < 0.01, "idf={idf}");
    }

    #[test]
    fn test_bm25_score_basic() {
        let s = scorer();
        let idf = s.idf(100, 10);
        let score = s.term_score(2, 10, 10.0, idf);
        assert!(score > 0.0);
    }

    #[test]
    fn test_short_doc_scores_higher() {
        let s = scorer();
        let idf = 1.0;
        // same tf, shorter doc should score higher
        let short = s.term_score(1, 5, 20.0, idf);
        let long = s.term_score(1, 40, 20.0, idf);
        assert!(short > long, "short={short}, long={long}");
    }
}
