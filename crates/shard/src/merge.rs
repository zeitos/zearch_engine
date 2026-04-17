use crate::segment_list::SegmentEntry;
use search_core::{Document, IndexSchema};
use search_index::{SegmentReader, SegmentWriter};
use search_query::SegmentStatistics;
use std::path::Path;

/// Tiered merge policy: select groups of segments to merge.
pub struct TieredMergePolicy {
    /// Minimum number of segments before a merge is triggered.
    pub min_segments_to_merge: usize,
    /// Maximum segments to merge in one operation.
    pub max_merge_at_once: usize,
}

impl Default for TieredMergePolicy {
    fn default() -> Self {
        Self { min_segments_to_merge: 3, max_merge_at_once: 10 }
    }
}

impl TieredMergePolicy {
    /// Returns a list of segment ID groups to merge, or empty if no merge needed.
    pub fn find_merges<'a>(&self, segments: &'a [SegmentEntry]) -> Vec<Vec<&'a str>> {
        if segments.len() < self.min_segments_to_merge {
            return vec![];
        }
        // Simple policy: merge the oldest N small segments into one.
        // Sort by doc count ascending (smallest first), take up to max_merge_at_once.
        let mut indexed: Vec<(usize, u32)> = segments
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.reader.meta.doc_count))
            .collect();
        indexed.sort_by_key(|(_, count)| *count);

        let to_merge: Vec<&str> = indexed
            .iter()
            .take(self.max_merge_at_once)
            .map(|(i, _)| segments[*i].segment_id.as_str())
            .collect();

        if to_merge.len() >= self.min_segments_to_merge {
            vec![to_merge]
        } else {
            vec![]
        }
    }
}

pub struct MergeScheduler {
    pub schema: IndexSchema,
}

impl MergeScheduler {
    pub fn new(schema: IndexSchema) -> Self {
        Self { schema }
    }

    /// Merge source segments into a new segment at new_dir.
    /// Deleted docs are excluded from the output.
    pub fn execute_merge(
        &self,
        sources: &[&SegmentEntry],
        new_segment_id: &str,
        new_dir: &Path,
    ) -> search_core::Result<SegmentEntry> {
        let mut merged_docs: Vec<Document> = Vec::new();

        for entry in sources {
            let reader: &SegmentReader = &entry.reader;
            for local_id in 0..reader.meta.doc_count {
                if let Some(doc) = reader.get_doc(local_id) {
                    merged_docs.push(doc);
                }
            }
        }

        SegmentWriter::new(self.schema.clone())
            .write(new_segment_id, &merged_docs, new_dir)
            .map_err(search_core::Error::Io)?;

        let reader = SegmentReader::open(new_dir).map_err(search_core::Error::Io)?;
        let stats = SegmentStatistics::compute(&reader, &self.schema);

        Ok(SegmentEntry {
            segment_id: new_segment_id.to_string(),
            dir: new_dir.to_path_buf(),
            reader,
            stats,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{Document, IndexSchema};
    use search_index::SegmentWriter;
    use std::collections::HashMap;

    fn make_doc(id: u64, title: &str) -> Document {
        Document {
            id,
            title: title.into(),
            description: "desc".into(),
            price: id as f64,
            category: "test".into(),
            attributes: HashMap::new(),
        }
    }

    fn write_entry(dir: &Path, seg_id: &str, docs: &[Document], schema: &IndexSchema) -> SegmentEntry {
        let seg_dir = dir.join(seg_id);
        SegmentWriter::new(schema.clone()).write(seg_id, docs, &seg_dir).unwrap();
        let reader = search_index::SegmentReader::open(&seg_dir).unwrap();
        let stats = SegmentStatistics::compute(&reader, schema);
        SegmentEntry { segment_id: seg_id.into(), dir: seg_dir, reader, stats }
    }

    #[test]
    fn test_tiered_policy_no_merge_needed() {
        let schema = IndexSchema::default_product_schema();
        let dir = tempfile::TempDir::new().unwrap();
        let docs = vec![make_doc(1, "Samsung")];
        let e1 = write_entry(dir.path(), "seg-000001", &docs, &schema);
        let e2 = write_entry(dir.path(), "seg-000002", &docs, &schema);
        let segments = vec![e1, e2];

        let policy = TieredMergePolicy { min_segments_to_merge: 3, max_merge_at_once: 10 };
        let merges = policy.find_merges(&segments);
        assert!(merges.is_empty());
    }

    #[test]
    fn test_tiered_policy_triggers_merge() {
        let schema = IndexSchema::default_product_schema();
        let dir = tempfile::TempDir::new().unwrap();
        let docs = vec![make_doc(1, "Samsung")];
        let e1 = write_entry(dir.path(), "seg-000001", &docs, &schema);
        let e2 = write_entry(dir.path(), "seg-000002", &docs, &schema);
        let e3 = write_entry(dir.path(), "seg-000003", &docs, &schema);
        let segments = vec![e1, e2, e3];

        let policy = TieredMergePolicy::default();
        let merges = policy.find_merges(&segments);
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0].len(), 3);
    }

    #[test]
    fn test_merge_execution() {
        let schema = IndexSchema::default_product_schema();
        let dir = tempfile::TempDir::new().unwrap();
        let docs1 = vec![make_doc(1, "Samsung"), make_doc(2, "Apple")];
        let docs2 = vec![make_doc(3, "Nike")];
        let e1 = write_entry(dir.path(), "seg-000001", &docs1, &schema);
        let e2 = write_entry(dir.path(), "seg-000002", &docs2, &schema);

        let scheduler = MergeScheduler::new(schema);
        let new_dir = dir.path().join("seg-000003");
        let merged = scheduler.execute_merge(&[&e1, &e2], "seg-000003", &new_dir).unwrap();

        assert_eq!(merged.reader.meta.doc_count, 3);
        assert!(merged.reader.get_doc(0).is_some());
        assert!(merged.reader.get_doc(2).is_some());
    }

    #[test]
    fn test_merge_excludes_deleted() {
        let schema = IndexSchema::default_product_schema();
        let dir = tempfile::TempDir::new().unwrap();
        let docs1 = vec![make_doc(1, "Samsung"), make_doc(2, "Apple")];
        let e1 = write_entry(dir.path(), "seg-000001", &docs1, &schema);

        // Mark doc 0 (local_id=0 = Samsung) as deleted
        search_index::DeletionBitmap::mark_deleted(&e1.dir, 0).unwrap();
        // Re-open to pick up deletion
        let reader = search_index::SegmentReader::open(&e1.dir).unwrap();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let e1 = SegmentEntry { segment_id: "seg-000001".into(), dir: e1.dir, reader, stats };

        let scheduler = MergeScheduler::new(schema);
        let new_dir = dir.path().join("seg-000002");
        let merged = scheduler.execute_merge(&[&e1], "seg-000002", &new_dir).unwrap();

        // Only 1 live doc should be in the merged segment
        assert_eq!(merged.reader.meta.doc_count, 1);
    }
}
