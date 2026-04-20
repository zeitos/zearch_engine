use search_analysis::{Analyzer, LowercaseFilter, UnicodeTokenizer};
use search_core::{Document, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::index::SuggestIndex;

/// Accumulates term -> doc_frequency from incoming documents.
/// No stemming: suggest works on surface forms.
/// Terms stored as `Arc<str>` so flushes can clone refcounts instead of bytes.
pub struct SuggestIndexBuilder {
    counts: HashMap<Arc<str>, u32>,
    analyzer: Analyzer,
}

impl Default for SuggestIndexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SuggestIndexBuilder {
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
            analyzer: Analyzer::new(Box::new(UnicodeTokenizer), vec![Box::new(LowercaseFilter)]),
        }
    }

    pub fn term_count(&self) -> usize {
        self.counts.len()
    }

    /// Build without consuming — takes a snapshot of current counts.
    /// Cloning `Arc<str>` is a refcount bump, not a heap copy.
    pub fn snapshot(&self, min_doc_frequency: u32, max_terms: u32) -> SuggestIndex {
        let mut filtered: Vec<(Arc<str>, u32)> = self
            .counts
            .iter()
            .filter(|(_, c)| **c >= min_doc_frequency)
            .map(|(t, c)| (Arc::clone(t), *c))
            .collect();

        if max_terms > 0 && filtered.len() > max_terms as usize {
            filtered.sort_by(|a, b| b.1.cmp(&a.1));
            filtered.truncate(max_terms as usize);
        }

        let max_freq = filtered.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1) as f32;
        let terms: Vec<(Arc<str>, f32)> = filtered
            .into_iter()
            .map(|(t, c)| (t, c as f32 / max_freq))
            .collect();

        SuggestIndex::new(terms)
    }

    pub fn add_document(&mut self, doc: &Document, fields: &[String]) {
        for field in fields {
            let text = extract_field(doc, field);
            if text.is_empty() {
                continue;
            }
            // Dedupe tokens via sort+dedup — avoids per-doc HashSet allocation
            // and the extra string clone the HashSet needed.
            let mut terms = self.analyzer.analyze_to_terms(&text);
            terms.sort_unstable();
            terms.dedup();
            for t in terms {
                if t.is_empty() {
                    continue;
                }
                // Lookup by &str avoids allocating Arc on repeat occurrences.
                if let Some(c) = self.counts.get_mut(t.as_str()) {
                    *c += 1;
                } else {
                    self.counts.insert(Arc::from(t), 1);
                }
            }
        }
    }

    /// Build a SuggestIndex. Drops terms below `min_doc_frequency`,
    /// keeps top `max_terms` by frequency, normalizes scores to [0,1].
    pub fn build(self, min_doc_frequency: u32, max_terms: u32) -> SuggestIndex {
        let mut filtered: Vec<(Arc<str>, u32)> = self
            .counts
            .into_iter()
            .filter(|(_, c)| *c >= min_doc_frequency)
            .collect();

        if max_terms > 0 && filtered.len() > max_terms as usize {
            filtered.sort_by(|a, b| b.1.cmp(&a.1));
            filtered.truncate(max_terms as usize);
        }

        let max_freq = filtered.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1) as f32;
        let terms: Vec<(Arc<str>, f32)> = filtered
            .into_iter()
            .map(|(t, c)| (t, c as f32 / max_freq))
            .collect();

        SuggestIndex::new(terms)
    }
}

fn extract_field<'a>(doc: &'a Document, field: &str) -> Cow<'a, str> {
    match field {
        "title" => Cow::Borrowed(doc.title.as_str()),
        "description" => Cow::Borrowed(doc.description.as_str()),
        "category" => Cow::Borrowed(doc.category.as_str()),
        other => match doc.attributes.get(other) {
            Some(Value::String(s)) => Cow::Borrowed(s.as_str()),
            Some(Value::StringArray(v)) => Cow::Owned(v.join(" ")),
            _ => Cow::Borrowed(""),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: u64, title: &str) -> Document {
        Document {
            id,
            title: title.into(),
            description: String::new(),
            price: 0.0,
            category: String::new(),
            attributes: HashMap::new(),
        }
    }

    #[test]
    fn counts_doc_frequency() {
        let mut b = SuggestIndexBuilder::new();
        let fields = vec!["title".to_string()];
        b.add_document(&doc(1, "wireless mouse"), &fields);
        b.add_document(&doc(2, "wireless keyboard"), &fields);
        b.add_document(&doc(3, "wired mouse"), &fields);
        let idx = b.build(1, 100);
        let r = idx.query("wire", 5);
        // "wireless" has freq 2, "wired" has freq 1 -> wireless first.
        assert_eq!(r[0].term, "wireless");
        assert_eq!(r[1].term, "wired");
        assert!(r[0].score >= r[1].score);
    }

    #[test]
    fn min_doc_frequency_filter() {
        let mut b = SuggestIndexBuilder::new();
        let fields = vec!["title".to_string()];
        b.add_document(&doc(1, "wireless"), &fields);
        b.add_document(&doc(2, "wired"), &fields);
        b.add_document(&doc(3, "wireless"), &fields);
        let idx = b.build(2, 100);
        assert_eq!(idx.query("wire", 5).len(), 1);
    }

    #[test]
    fn term_counted_once_per_doc() {
        let mut b = SuggestIndexBuilder::new();
        let fields = vec!["title".to_string()];
        b.add_document(&doc(1, "wireless wireless wireless"), &fields);
        let idx = b.build(1, 100);
        assert_eq!(idx.query("wireless", 5).len(), 1);
        // Single doc -> normalized score = 1.0
        assert!((idx.query("wireless", 5)[0].score - 1.0).abs() < 1e-6);
    }
}
