use search_core::{Document, SuggestShardConfig};
use std::sync::{Arc, RwLock};

use crate::builder::SuggestIndexBuilder;
use crate::index::SuggestIndex;
use crate::term::SuggestTerm;

/// In-memory, lossy suggest engine. No WAL, no persistence.
pub struct SuggestShardEngine {
    config: SuggestShardConfig,
    /// Write buffer: accumulates term frequencies as docs arrive.
    builder: RwLock<SuggestIndexBuilder>,
    /// Current queryable index. Rebuilt on `flush()`.
    index: RwLock<Arc<SuggestIndex>>,
}

impl SuggestShardEngine {
    pub fn new(config: SuggestShardConfig) -> Self {
        Self {
            config,
            builder: RwLock::new(SuggestIndexBuilder::new()),
            index: RwLock::new(Arc::new(SuggestIndex::default())),
        }
    }

    pub fn config(&self) -> &SuggestShardConfig {
        &self.config
    }

    pub fn index(&self, doc: &Document) {
        self.builder.write().unwrap().add_document(doc, &self.config.fields);
    }

    pub fn bulk(&self, docs: &[Document]) {
        let mut b = self.builder.write().unwrap();
        for doc in docs {
            b.add_document(doc, &self.config.fields);
        }
    }

    /// Snapshots the current builder state into a fresh queryable index.
    /// The builder is NOT cleared — it accumulates across flushes.
    pub fn flush(&self) -> usize {
        let new_index = self.builder.read().unwrap().snapshot(
            self.config.min_doc_frequency,
            self.config.max_terms_per_shard,
        );
        let len = new_index.len();
        *self.index.write().unwrap() = Arc::new(new_index);
        len
    }

    pub fn suggest(&self, prefix: &str, _field: &str, limit: usize) -> Vec<SuggestTerm> {
        let idx = self.index.read().unwrap().clone();
        idx.query(prefix, limit)
    }

    pub fn term_count(&self) -> usize {
        self.index.read().unwrap().len()
    }

    pub fn pending_terms(&self) -> usize {
        self.builder.read().unwrap().term_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

    fn cfg() -> SuggestShardConfig {
        SuggestShardConfig {
            min_doc_frequency: 1,
            max_terms_per_shard: 100,
            fields: vec!["title".into()],
            ..SuggestShardConfig::default()
        }
    }

    #[test]
    fn index_then_flush_then_query() {
        let eng = SuggestShardEngine::new(cfg());
        eng.index(&doc(1, "wireless mouse"));
        eng.index(&doc(2, "wired mouse"));
        // Before flush, index is empty.
        assert!(eng.suggest("wire", "title", 5).is_empty());
        eng.flush();
        let r = eng.suggest("wire", "title", 5);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn builder_persists_across_flushes() {
        let eng = SuggestShardEngine::new(cfg());
        eng.index(&doc(1, "wireless"));
        eng.flush();
        eng.index(&doc(2, "wired"));
        eng.flush();
        let r = eng.suggest("wire", "title", 5);
        assert_eq!(r.len(), 2);
    }
}
