use crate::column::{
    ColumnStore, KeywordColumnWriter, NumericColumnWriter,
};
use crate::docstore::{DocStoreReader, DocStoreWriter};
use crate::inverted::{InvertedIndexReader, InvertedIndexWriter};
use crate::posting::PostingList;
use roaring::RoaringBitmap;
use search_analysis::{Analyzer, AnalyzerFactory};
use search_core::{Document, FieldType, IndexSchema};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// Metadata for a segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub doc_count: u32,
    pub term_count: usize,
    pub segment_id: String,
}

impl SegmentMeta {
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        std::fs::write(dir.join("meta.json"), json)
    }

    pub fn read(dir: &Path) -> io::Result<Self> {
        let data = std::fs::read_to_string(dir.join("meta.json"))?;
        serde_json::from_str(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// Writes a complete segment from a batch of documents.
pub struct SegmentWriter {
    schema: IndexSchema,
}

impl SegmentWriter {
    pub fn new(schema: IndexSchema) -> Self {
        Self { schema }
    }

    /// Write a batch of documents as a new segment.
    /// `base_doc_id` is the starting local doc_id within this segment (usually 0).
    pub fn write(&self, segment_id: &str, docs: &[Document], dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;

        let mut inverted_writer = InvertedIndexWriter::new();
        let mut doc_writer = DocStoreWriter::new();

        // Column writers per field
        let mut keyword_writers: Vec<(&str, KeywordColumnWriter)> = Vec::new();
        let mut numeric_writers: Vec<(&str, NumericColumnWriter)> = Vec::new();

        for field in &self.schema.fields {
            match field.field_type {
                FieldType::Keyword if field.filterable || field.aggregatable => {
                    keyword_writers.push((&field.name, KeywordColumnWriter::new()));
                }
                FieldType::Numeric if field.filterable => {
                    numeric_writers.push((&field.name, NumericColumnWriter::new()));
                }
                _ => {}
            }
        }

        // Build analyzers for indexed text fields
        let text_analyzers: Vec<(&str, Analyzer, f32)> = self
            .schema
            .fields
            .iter()
            .filter(|f| f.indexed && f.field_type == FieldType::Text)
            .map(|f| {
                let analyzer = AnalyzerFactory::build(&f.analyzer);
                (f.name.as_str(), analyzer, f.boost)
            })
            .collect();

        for (local_doc_id, doc) in docs.iter().enumerate() {
            let local_id = local_doc_id as u32;

            // Index text fields
            for (field_name, analyzer, _boost) in &text_analyzers {
                let text = match *field_name {
                    "title" => &doc.title,
                    "description" => &doc.description,
                    _ => continue,
                };
                let tokens = analyzer.analyze(text);
                for token in &tokens {
                    inverted_writer.add_term(&token.text, local_id);
                }
            }

            // Column store fields
            for (field_name, writer) in &mut keyword_writers {
                match *field_name {
                    "category" => writer.add(&doc.category),
                    other => {
                        if let Some(search_core::Value::String(v)) = doc.attributes.get(other) {
                            writer.add(v);
                        } else {
                            writer.add_missing();
                        }
                    }
                }
            }

            for (field_name, writer) in &mut numeric_writers {
                match *field_name {
                    "price" => writer.add(doc.price),
                    other => {
                        if let Some(search_core::Value::Number(v)) = doc.attributes.get(other) {
                            writer.add(*v);
                        } else {
                            writer.add_missing();
                        }
                    }
                }
            }

            // Doc store
            doc_writer.add(doc.clone());
        }

        // Write inverted index
        inverted_writer.write(dir)?;

        // Write column store
        let mut col_store = ColumnStore::default();
        for (name, writer) in keyword_writers {
            col_store
                .keyword_columns
                .insert(name.to_string(), writer.build());
        }
        for (name, writer) in numeric_writers {
            col_store
                .numeric_columns
                .insert(name.to_string(), writer.build());
        }
        col_store.write(dir)?;

        // Write doc store
        doc_writer.write(dir)?;

        // Write metadata
        let meta = SegmentMeta {
            doc_count: docs.len() as u32,
            term_count: inverted_writer.term_count(),
            segment_id: segment_id.to_string(),
        };
        meta.write(dir)?;

        Ok(())
    }
}

/// Reads a segment from disk, providing search and retrieval operations.
pub struct SegmentReader {
    pub dir: PathBuf,
    pub meta: SegmentMeta,
    pub inverted: InvertedIndexReader,
    pub columns: ColumnStore,
    pub docs: DocStoreReader,
}

impl SegmentReader {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let meta = SegmentMeta::read(dir)?;
        let inverted = InvertedIndexReader::open(dir)?;
        let columns = ColumnStore::read(dir)?;
        let docs = DocStoreReader::open(dir)?;

        Ok(Self {
            dir: dir.to_path_buf(),
            meta,
            inverted,
            columns,
            docs,
        })
    }

    /// Search for a term and return matching posting list (excluding deleted docs).
    pub fn search_term(&self, term: &str) -> Option<PostingList> {
        let mut pl = self.inverted.get_postings(term)?;
        let deletions = self.docs.deletion_bitmap();
        if !deletions.is_empty() {
            pl.postings.retain(|p| !deletions.contains(p.doc_id));
        }
        Some(pl)
    }

    /// Prefix search — returns all terms starting with `prefix`.
    pub fn prefix_search_term(&self, prefix: &str) -> Vec<(String, PostingList)> {
        let deletions = self.docs.deletion_bitmap();
        self.inverted
            .prefix_search(prefix)
            .into_iter()
            .map(|(term, mut pl)| {
                if !deletions.is_empty() {
                    pl.postings.retain(|p| !deletions.contains(p.doc_id));
                }
                (term, pl)
            })
            .collect()
    }

    /// Fuzzy search for a term.
    pub fn fuzzy_search_term(
        &self,
        term: &str,
        max_distance: u32,
    ) -> Vec<(String, PostingList)> {
        let deletions = self.docs.deletion_bitmap();
        self.inverted
            .fuzzy_search(term, max_distance)
            .into_iter()
            .map(|(t, mut pl)| {
                if !deletions.is_empty() {
                    pl.postings.retain(|p| !deletions.contains(p.doc_id));
                }
                (t, pl)
            })
            .filter(|(_, pl)| !pl.is_empty())
            .collect()
    }

    /// Get a document by local doc_id.
    pub fn get_doc(&self, local_doc_id: u32) -> Option<Document> {
        self.docs.get(local_doc_id)
    }

    /// Apply filters and return matching doc IDs.
    pub fn filter_eq(&self, field: &str, value: &str) -> RoaringBitmap {
        let mut result = self.columns.filter_eq(field, value);
        result -= self.docs.deletion_bitmap();
        result
    }

    pub fn filter_range(&self, field: &str, gte: Option<f64>, lte: Option<f64>) -> RoaringBitmap {
        let mut result = self.columns.filter_range(field, gte, lte);
        result -= self.docs.deletion_bitmap();
        result
    }

    pub fn filter_in(&self, field: &str, values: &[String]) -> RoaringBitmap {
        let mut result = self.columns.filter_in(field, values);
        result -= self.docs.deletion_bitmap();
        result
    }

    /// Count aggregation on a field for matching docs.
    pub fn aggregate_counts(
        &self,
        field: &str,
        matching_docs: &RoaringBitmap,
    ) -> Vec<(String, u64)> {
        self.columns.aggregate_counts(field, matching_docs)
    }

    /// Return a bitmap of all live (non-deleted) doc IDs.
    pub fn all_live_docs(&self) -> RoaringBitmap {
        let mut all = RoaringBitmap::new();
        for i in 0..self.meta.doc_count {
            all.insert(i);
        }
        all -= self.docs.deletion_bitmap();
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docstore::DeletionBitmap;
    use std::collections::HashMap;

    fn test_schema() -> IndexSchema {
        IndexSchema::default_product_schema()
    }

    fn make_docs(n: usize) -> Vec<Document> {
        (0..n)
            .map(|i| {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "brand".to_string(),
                    search_core::Value::String(
                        ["samsung", "apple", "nike", "adidas"][i % 4].to_string(),
                    ),
                );
                Document {
                    id: i as u64,
                    title: format!("Product {} Samsung Galaxy", i),
                    description: format!("This is product number {} with great features", i),
                    price: (i as f64) * 100.0 + 9.99,
                    category: ["electronics", "clothing"][i % 2].to_string(),
                    attributes: attrs,
                }
            })
            .collect()
    }

    #[test]
    fn test_segment_write_and_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg-0001");

        let schema = test_schema();
        let docs = make_docs(100);

        let writer = SegmentWriter::new(schema);
        writer.write("seg-0001", &docs, &seg_dir).unwrap();

        let reader = SegmentReader::open(&seg_dir).unwrap();
        assert_eq!(reader.meta.doc_count, 100);
        assert_eq!(reader.meta.segment_id, "seg-0001");

        // Search for a term that should be in every title
        let pl = reader.search_term("samsung").unwrap();
        assert!(!pl.is_empty());

        // Get a document
        let doc = reader.get_doc(0).unwrap();
        assert_eq!(doc.id, 0);
        assert!(doc.title.contains("Samsung"));
    }

    #[test]
    fn test_segment_filters() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");

        let schema = test_schema();
        let docs = make_docs(10);

        let writer = SegmentWriter::new(schema);
        writer.write("seg", &docs, &seg_dir).unwrap();

        let reader = SegmentReader::open(&seg_dir).unwrap();

        // Filter by category
        let electronics = reader.filter_eq("category", "electronics");
        assert_eq!(electronics.len(), 5); // even indices

        // Filter by price range
        let expensive = reader.filter_range("price", Some(500.0), None);
        assert!(!expensive.is_empty());
    }

    #[test]
    fn test_segment_aggregations() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");

        let schema = test_schema();
        let docs = make_docs(20);

        let writer = SegmentWriter::new(schema);
        writer.write("seg", &docs, &seg_dir).unwrap();

        let reader = SegmentReader::open(&seg_dir).unwrap();
        let all = reader.all_live_docs();

        let counts = reader.aggregate_counts("category", &all);
        let electronics = counts.iter().find(|(v, _)| v == "electronics").unwrap();
        let clothing = counts.iter().find(|(v, _)| v == "clothing").unwrap();
        assert_eq!(electronics.1, 10);
        assert_eq!(clothing.1, 10);
    }

    #[test]
    fn test_segment_with_deletions() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");

        let schema = test_schema();
        let docs = make_docs(10);

        let writer = SegmentWriter::new(schema);
        writer.write("seg", &docs, &seg_dir).unwrap();

        // Delete docs 0 and 1
        DeletionBitmap::mark_deleted(&seg_dir, 0).unwrap();
        DeletionBitmap::mark_deleted(&seg_dir, 1).unwrap();

        // Reopen
        let reader = SegmentReader::open(&seg_dir).unwrap();
        assert_eq!(reader.docs.live_doc_count(), 8);

        assert!(reader.get_doc(0).is_none());
        assert!(reader.get_doc(1).is_none());
        assert!(reader.get_doc(2).is_some());

        // Filters should exclude deleted docs
        let all = reader.all_live_docs();
        assert_eq!(all.len(), 8);
        assert!(!all.contains(0));
        assert!(!all.contains(1));
    }

    #[test]
    fn test_segment_fuzzy_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");

        let schema = test_schema();
        let docs = make_docs(5);

        let writer = SegmentWriter::new(schema);
        writer.write("seg", &docs, &seg_dir).unwrap();

        let reader = SegmentReader::open(&seg_dir).unwrap();

        // Typo: "samsnug" should match "samsung" with distance 2
        // ("samsnug" is 2 edits from "samsung": u→n, n→u)
        let results = reader.fuzzy_search_term("samsnug", 2);
        let terms: Vec<&str> = results.iter().map(|(t, _)| t.as_str()).collect();
        assert!(terms.contains(&"samsung"));
    }

    #[test]
    fn test_segment_1000_docs() {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");

        let schema = test_schema();
        let docs = make_docs(1000);

        let writer = SegmentWriter::new(schema);
        writer.write("seg", &docs, &seg_dir).unwrap();

        let reader = SegmentReader::open(&seg_dir).unwrap();
        assert_eq!(reader.meta.doc_count, 1000);

        // Verify search works
        let pl = reader.search_term("samsung").unwrap();
        assert_eq!(pl.len(), 1000); // every doc has "Samsung" in title

        // Verify doc retrieval
        let doc = reader.get_doc(999).unwrap();
        assert_eq!(doc.id, 999);
    }
}
