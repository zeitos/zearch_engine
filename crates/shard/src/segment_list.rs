use search_core::{IndexSchema};
use search_index::{DeletionBitmap, SegmentReader};
use search_query::SegmentStatistics;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct SegmentEntry {
    pub segment_id: String,
    pub dir: PathBuf,
    pub reader: SegmentReader,
    pub stats: SegmentStatistics,
}

/// Location of a document (global doc_id) within the shard.
#[derive(Clone)]
pub struct DocLocation {
    pub segment_id: String,
    pub local_doc_id: u32,
}

pub struct SegmentList {
    segments: Vec<SegmentEntry>,
    /// global doc_id -> location
    doc_id_map: HashMap<u64, DocLocation>,
    data_dir: PathBuf,
    next_segment_seq: u64,
}

impl SegmentList {
    /// Open existing segments from the data directory.
    pub fn open(data_dir: &Path, schema: &IndexSchema) -> search_core::Result<Self> {
        std::fs::create_dir_all(data_dir).map_err(search_core::Error::Io)?;

        let mut segments = Vec::new();
        let mut doc_id_map = HashMap::new();
        let mut max_seq: u64 = 0;

        if let Ok(entries) = std::fs::read_dir(data_dir) {
            let mut dirs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir() && p.file_name().map(|n| n.to_string_lossy().starts_with("seg-")).unwrap_or(false))
                .collect();
            dirs.sort();

            for dir in dirs {
                let seg_id = dir.file_name().unwrap().to_string_lossy().to_string();
                // Parse sequence number from seg-NNNN
                if let Some(seq_str) = seg_id.strip_prefix("seg-") {
                    if let Ok(seq) = seq_str.parse::<u64>() {
                        max_seq = max_seq.max(seq);
                    }
                }

                match SegmentReader::open(&dir) {
                    Ok(reader) => {
                        // Build doc_id map for this segment
                        for local_id in 0..reader.meta.doc_count {
                            if let Some(doc) = reader.get_doc(local_id) {
                                doc_id_map.insert(doc.id, DocLocation {
                                    segment_id: seg_id.clone(),
                                    local_doc_id: local_id,
                                });
                            }
                        }
                        let stats = SegmentStatistics::compute(&reader, schema);
                        segments.push(SegmentEntry { segment_id: seg_id, dir, reader, stats });
                    }
                    Err(_) => {
                        tracing::warn!("Failed to open segment at {:?}, skipping", dir);
                    }
                }
            }
        }

        Ok(Self {
            segments,
            doc_id_map,
            data_dir: data_dir.to_path_buf(),
            next_segment_seq: max_seq + 1,
        })
    }

    /// Generate the next segment ID.
    pub fn next_segment_id(&mut self) -> String {
        let id = format!("seg-{:06}", self.next_segment_seq);
        self.next_segment_seq += 1;
        id
    }

    /// Add a newly flushed segment and update the doc_id map.
    pub fn add_segment(&mut self, entry: SegmentEntry) {
        for local_id in 0..entry.reader.meta.doc_count {
            if let Some(doc) = entry.reader.get_doc(local_id) {
                self.doc_id_map.insert(doc.id, DocLocation {
                    segment_id: entry.segment_id.clone(),
                    local_doc_id: local_id,
                });
            }
        }
        self.segments.push(entry);
    }

    /// Replace source segments with a merged one.
    pub fn replace_segments(&mut self, old_ids: &[String], new_entry: SegmentEntry) {
        // Remove old mappings
        self.doc_id_map.retain(|_, loc| !old_ids.contains(&loc.segment_id));
        // Add new mappings
        for local_id in 0..new_entry.reader.meta.doc_count {
            if let Some(doc) = new_entry.reader.get_doc(local_id) {
                self.doc_id_map.insert(doc.id, DocLocation {
                    segment_id: new_entry.segment_id.clone(),
                    local_doc_id: local_id,
                });
            }
        }
        self.segments.retain(|s| !old_ids.contains(&s.segment_id));
        self.segments.push(new_entry);
    }

    /// Borrow all segment readers + stats for searching.
    pub fn readers(&self) -> Vec<(&SegmentReader, &SegmentStatistics)> {
        self.segments.iter().map(|e| (&e.reader, &e.stats)).collect()
    }

    /// Find a doc by global doc_id and mark it deleted.
    pub fn mark_deleted(&mut self, doc_id: u64) -> search_core::Result<bool> {
        let Some(loc) = self.doc_id_map.get(&doc_id).cloned() else {
            return Ok(false);
        };
        let Some(entry) = self.segments.iter().find(|s| s.segment_id == loc.segment_id) else {
            return Ok(false);
        };
        DeletionBitmap::mark_deleted(&entry.dir, loc.local_doc_id)
            .map_err(search_core::Error::Io)?;
        self.doc_id_map.remove(&doc_id);
        Ok(true)
    }

    /// Get a document by global doc_id.
    pub fn get_doc(&self, doc_id: u64) -> Option<search_core::Document> {
        let loc = self.doc_id_map.get(&doc_id)?;
        let entry = self.segments.iter().find(|s| s.segment_id == loc.segment_id)?;
        entry.reader.get_doc(loc.local_doc_id)
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn total_live_docs(&self) -> u64 {
        self.segments.iter().map(|e| e.reader.all_live_docs().len()).sum()
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn segments(&self) -> &[SegmentEntry] {
        &self.segments
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
            price: 0.0,
            category: "test".into(),
            attributes: HashMap::new(),
        }
    }

    fn write_segment(dir: &Path, seg_id: &str, docs: &[Document], schema: &IndexSchema) -> SegmentEntry {
        let seg_dir = dir.join(seg_id);
        SegmentWriter::new(schema.clone()).write(seg_id, docs, &seg_dir).unwrap();
        let reader = SegmentReader::open(&seg_dir).unwrap();
        let stats = SegmentStatistics::compute(&reader, schema);
        SegmentEntry { segment_id: seg_id.into(), dir: seg_dir, reader, stats }
    }

    #[test]
    fn test_open_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let list = SegmentList::open(dir.path(), &schema).unwrap();
        assert_eq!(list.segment_count(), 0);
        assert_eq!(list.total_live_docs(), 0);
    }

    #[test]
    fn test_add_and_get_doc() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let mut list = SegmentList::open(dir.path(), &schema).unwrap();

        let docs = vec![make_doc(1, "Samsung"), make_doc(2, "Apple")];
        let entry = write_segment(dir.path(), "seg-000001", &docs, &schema);
        list.add_segment(entry);

        assert_eq!(list.segment_count(), 1);
        assert_eq!(list.total_live_docs(), 2);
        assert!(list.get_doc(1).is_some());
        assert!(list.get_doc(99).is_none());
    }

    #[test]
    fn test_mark_deleted() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let mut list = SegmentList::open(dir.path(), &schema).unwrap();

        let docs = vec![make_doc(1, "Samsung"), make_doc(2, "Apple")];
        let entry = write_segment(dir.path(), "seg-000001", &docs, &schema);
        list.add_segment(entry);

        let found = list.mark_deleted(1).unwrap();
        assert!(found);
        assert!(list.get_doc(1).is_none()); // removed from map
    }

    #[test]
    fn test_replace_segments() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let mut list = SegmentList::open(dir.path(), &schema).unwrap();

        let docs1 = vec![make_doc(1, "Samsung")];
        let docs2 = vec![make_doc(2, "Apple")];
        let e1 = write_segment(dir.path(), "seg-000001", &docs1, &schema);
        let e2 = write_segment(dir.path(), "seg-000002", &docs2, &schema);
        list.add_segment(e1);
        list.add_segment(e2);
        assert_eq!(list.segment_count(), 2);

        let merged_docs = vec![make_doc(1, "Samsung"), make_doc(2, "Apple")];
        let merged = write_segment(dir.path(), "seg-000003", &merged_docs, &schema);
        list.replace_segments(&["seg-000001".into(), "seg-000002".into()], merged);
        assert_eq!(list.segment_count(), 1);
        assert!(list.get_doc(1).is_some());
        assert!(list.get_doc(2).is_some());
    }

    #[test]
    fn test_open_existing_segments() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        {
            // Write segments to disk
            let docs = vec![make_doc(10, "Samsung"), make_doc(20, "Apple")];
            let seg_dir = dir.path().join("seg-000001");
            SegmentWriter::new(schema.clone()).write("seg-000001", &docs, &seg_dir).unwrap();
        }
        // Re-open — should discover the segment
        let list = SegmentList::open(dir.path(), &schema).unwrap();
        assert_eq!(list.segment_count(), 1);
        assert!(list.get_doc(10).is_some());
    }
}
