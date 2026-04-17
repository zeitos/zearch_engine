use search_core::{Document, IndexSchema};
use search_index::{SegmentReader, SegmentWriter};
use search_query::SegmentStatistics;
use std::collections::HashSet;

pub struct WriteBuffer {
    schema: IndexSchema,
    docs: Vec<Document>,
    deleted_ids: HashSet<u64>,
    capacity_bytes: usize,
    current_size_bytes: usize,
    segment_dir: Option<tempfile::TempDir>,
    segment_reader: Option<SegmentReader>,
    segment_stats: Option<SegmentStatistics>,
    dirty: bool,
}

impl WriteBuffer {
    pub fn new(schema: IndexSchema, capacity_bytes: usize) -> Self {
        Self {
            schema,
            docs: Vec::new(),
            deleted_ids: HashSet::new(),
            capacity_bytes,
            current_size_bytes: 0,
            segment_dir: None,
            segment_reader: None,
            segment_stats: None,
            dirty: false,
        }
    }

    /// Add a document. Returns true if the buffer is now at or above capacity.
    pub fn add(&mut self, doc: Document) -> bool {
        self.current_size_bytes +=
            doc.title.len() + doc.description.len() + doc.category.len() + 64;
        self.docs.push(doc);
        self.dirty = true;
        self.is_full()
    }

    /// Mark a document as deleted in the buffer (by global doc_id).
    pub fn mark_deleted(&mut self, doc_id: u64) {
        self.deleted_ids.insert(doc_id);
        self.dirty = true;
    }

    pub fn is_full(&self) -> bool {
        self.current_size_bytes >= self.capacity_bytes
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    pub fn size_bytes(&self) -> usize {
        self.current_size_bytes
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Returns a SegmentReader + SegmentStatistics for searching the buffer.
    /// Rebuilds the temp segment if new docs were added since last build.
    pub fn reader_and_stats(
        &mut self,
    ) -> search_core::Result<Option<(&SegmentReader, &SegmentStatistics)>> {
        if self.docs.is_empty() {
            return Ok(None);
        }
        if self.dirty {
            self.rebuild_segment()?;
            self.dirty = false;
        }
        match (&self.segment_reader, &self.segment_stats) {
            (Some(r), Some(s)) => Ok(Some((r, s))),
            _ => Ok(None),
        }
    }

    fn rebuild_segment(&mut self) -> search_core::Result<()> {
        // Drop old temp dir
        self.segment_reader = None;
        self.segment_stats = None;
        self.segment_dir = None;

        let temp_dir =
            tempfile::TempDir::new().map_err(search_core::Error::Io)?;
        let seg_path = temp_dir.path().join("buf");

        // Filter out deleted docs
        let live_docs: Vec<&Document> = self
            .docs
            .iter()
            .filter(|d| !self.deleted_ids.contains(&d.id))
            .collect();

        if live_docs.is_empty() {
            self.segment_dir = Some(temp_dir);
            return Ok(());
        }

        let owned_docs: Vec<Document> = live_docs.into_iter().cloned().collect();
        SegmentWriter::new(self.schema.clone())
            .write("buffer", &owned_docs, &seg_path)
            .map_err(search_core::Error::Io)?;

        let reader =
            SegmentReader::open(&seg_path).map_err(search_core::Error::Io)?;
        let stats = SegmentStatistics::compute(&reader, &self.schema);

        self.segment_dir = Some(temp_dir);
        self.segment_reader = Some(reader);
        self.segment_stats = Some(stats);
        Ok(())
    }

    /// Iterate over all buffered documents (including deleted ones — caller must check).
    pub fn docs_iter(&self) -> impl Iterator<Item = &Document> {
        self.docs.iter()
    }

    /// Drain the buffer: return all docs and deleted IDs, reset to empty.
    pub fn drain(&mut self) -> (Vec<Document>, HashSet<u64>) {
        self.segment_reader = None;
        self.segment_stats = None;
        self.segment_dir = None;
        self.dirty = false;
        self.current_size_bytes = 0;
        let docs = std::mem::take(&mut self.docs);
        let deleted = std::mem::take(&mut self.deleted_ids);
        (docs, deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{IndexSchema, SearchRequest};
    use search_query::MultiSegmentSearcher;
    use std::collections::HashMap;

    fn make_doc(id: u64, title: &str) -> Document {
        Document {
            id,
            title: title.into(),
            description: "test desc".into(),
            price: id as f64 * 10.0,
            category: "electronics".into(),
            attributes: HashMap::new(),
        }
    }

    #[test]
    fn test_add_and_search() {
        let schema = IndexSchema::default_product_schema();
        let mut buf = WriteBuffer::new(schema.clone(), 1024 * 1024);
        buf.add(make_doc(1, "Samsung Galaxy"));
        buf.add(make_doc(2, "Apple iPhone"));

        let (reader, stats) = buf.reader_and_stats().unwrap().unwrap();
        let searcher = MultiSegmentSearcher::new(schema);
        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = searcher.search(&[(reader, stats)], &req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 1);
    }

    #[test]
    fn test_capacity() {
        let schema = IndexSchema::default_product_schema();
        // Small capacity: 100 bytes
        let mut buf = WriteBuffer::new(schema, 100);
        let full = buf.add(make_doc(1, "Samsung Galaxy phone with great camera and display"));
        assert!(full, "should be full after adding large doc");
    }

    #[test]
    fn test_drain() {
        let schema = IndexSchema::default_product_schema();
        let mut buf = WriteBuffer::new(schema, 1024 * 1024);
        buf.add(make_doc(1, "Samsung"));
        buf.add(make_doc(2, "Apple"));

        let (docs, _deleted) = buf.drain();
        assert_eq!(docs.len(), 2);
        assert!(buf.is_empty());
        assert_eq!(buf.doc_count(), 0);
    }

    #[test]
    fn test_delete_in_buffer() {
        let schema = IndexSchema::default_product_schema();
        let mut buf = WriteBuffer::new(schema.clone(), 1024 * 1024);
        buf.add(make_doc(1, "Samsung Galaxy"));
        buf.add(make_doc(2, "Apple iPhone"));
        buf.mark_deleted(1);

        let (reader, stats) = buf.reader_and_stats().unwrap().unwrap();
        let searcher = MultiSegmentSearcher::new(schema);
        let req = SearchRequest { query: "".into(), limit: 10, ..Default::default() };
        let resp = searcher.search(&[(reader, stats)], &req).unwrap();
        // Only doc 2 should be returned (doc 1 deleted)
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 2);
    }
}
