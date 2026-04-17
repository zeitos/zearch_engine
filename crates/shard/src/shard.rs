use crate::merge::{MergeScheduler, TieredMergePolicy};
use crate::segment_list::{SegmentEntry, SegmentList};
use crate::wal::{WalEntry, WriteAheadLog};
use crate::write_buffer::WriteBuffer;
use search_core::{Document, IndexSchema, SearchRequest, SearchResponse, ShardConfig};
use search_index::SegmentWriter;
use search_query::MultiSegmentSearcher;
use std::sync::Mutex;
use std::path::PathBuf;

pub struct ShardStats {
    pub doc_count: u64,
    pub segment_count: u32,
    pub buffer_doc_count: usize,
    pub buffer_size_bytes: usize,
    pub shard_id: u32,
}

pub struct ShardEngine {
    config: ShardConfig,
    schema: IndexSchema,
    wal: Mutex<WriteAheadLog>,
    write_buffer: std::sync::RwLock<WriteBuffer>,
    segments: std::sync::RwLock<SegmentList>,
    searcher: MultiSegmentSearcher,
    merge_scheduler: MergeScheduler,
    merge_policy: TieredMergePolicy,
}

impl ShardEngine {
    /// Open or create a shard. Replays WAL if entries exist.
    pub fn open(config: ShardConfig, schema: IndexSchema) -> search_core::Result<Self> {
        let data_dir = &config.data_dir;
        std::fs::create_dir_all(data_dir).map_err(search_core::Error::Io)?;

        let wal_path = data_dir.join("wal.bin");
        let mut wal = WriteAheadLog::open(&wal_path)?;

        // Load existing segments from disk
        let segments_dir = data_dir.join("segments");
        let mut segments = SegmentList::open(&segments_dir, &schema)?;

        // Replay WAL into write buffer
        let mut buffer = WriteBuffer::new(schema.clone(), config.write_buffer_size);
        let wal_entries = wal.read_all()?;
        for entry in wal_entries {
            match entry {
                WalEntry::Index(doc) => { buffer.add(doc); }
                WalEntry::Delete(doc_id) => {
                    buffer.mark_deleted(doc_id);
                    let _ = segments.mark_deleted(doc_id);
                }
            }
        }

        // Pre-warm FST term dictionaries — iterates all segment term maps
        // so the data is in the OS page cache before the first query.
        for entry in segments.segments() {
            let _ = entry.reader.inverted.term_count();
        }

        let searcher = MultiSegmentSearcher::new(schema.clone());
        let merge_scheduler = MergeScheduler::new(schema.clone());

        Ok(Self {
            config,
            schema,
            wal: Mutex::new(wal),
            write_buffer: std::sync::RwLock::new(buffer),
            segments: std::sync::RwLock::new(segments),
            searcher,
            merge_scheduler,
            merge_policy: TieredMergePolicy::default(),
        })
    }

    /// Index a document: append to WAL, add to write buffer, flush if full.
    pub fn index(&self, doc: Document) -> search_core::Result<()> {
        {
            let mut wal = self.wal.lock().unwrap();
            wal.append(&WalEntry::Index(doc.clone()))?;
        }
        let full = {
            let mut buf = self.write_buffer.write().unwrap();
            buf.add(doc)
        };
        if full {
            self.flush()?;
        }
        Ok(())
    }

    /// Index a batch of documents: single WAL fsync for the whole batch.
    pub fn index_batch(&self, docs: Vec<Document>) -> search_core::Result<()> {
        let wal_entries: Vec<WalEntry> = docs.iter().map(|d| WalEntry::Index(d.clone())).collect();
        {
            let mut wal = self.wal.lock().unwrap();
            wal.append_batch(&wal_entries)?;
        }
        let mut needs_flush = false;
        {
            let mut buf = self.write_buffer.write().unwrap();
            for doc in docs {
                if buf.add(doc) {
                    needs_flush = true;
                }
            }
        }
        if needs_flush {
            self.flush()?;
        }
        Ok(())
    }

    /// Delete a document by global doc_id.
    pub fn delete(&self, doc_id: u64) -> search_core::Result<()> {
        {
            let mut wal = self.wal.lock().unwrap();
            wal.append(&WalEntry::Delete(doc_id))?;
        }
        {
            let mut buf = self.write_buffer.write().unwrap();
            buf.mark_deleted(doc_id);
        }
        {
            let mut segs = self.segments.write().unwrap();
            segs.mark_deleted(doc_id)?;
        }
        Ok(())
    }

    /// Search across write buffer + all segments.
    pub fn search(&self, request: SearchRequest) -> search_core::Result<SearchResponse> {
        let segs = self.segments.read().unwrap();
        let mut buf = self.write_buffer.write().unwrap();

        let mut combined: Vec<(&search_index::SegmentReader, &search_query::SegmentStatistics)> =
            segs.readers();

        let buf_reader = buf.reader_and_stats()?;
        if let Some((br, bs)) = buf_reader {
            combined.push((br, bs));
        }

        self.searcher.search(&combined, &request)
    }

    /// Get documents by global doc_id.
    pub fn get_docs(&self, doc_ids: &[u64]) -> search_core::Result<Vec<Document>> {
        let segs = self.segments.read().unwrap();
        let buf = self.write_buffer.read().unwrap();

        let mut result = Vec::new();
        for &id in doc_ids {
            if let Some(doc) = segs.get_doc(id) {
                result.push(doc);
            } else {
                // Also check write buffer
                for d in buf.docs_iter() {
                    if d.id == id {
                        result.push(d.clone());
                        break;
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn stats(&self) -> ShardStats {
        let segs = self.segments.read().unwrap();
        let buf = self.write_buffer.read().unwrap();
        ShardStats {
            doc_count: segs.total_live_docs() + buf.doc_count() as u64,
            segment_count: segs.segment_count() as u32,
            buffer_doc_count: buf.doc_count(),
            buffer_size_bytes: buf.size_bytes(),
            shard_id: self.config.shard_id,
        }
    }

    /// Flush the write buffer to a new on-disk segment.
    pub fn flush(&self) -> search_core::Result<()> {
        let (docs, _deleted_ids) = {
            let mut buf = self.write_buffer.write().unwrap();
            buf.drain()
        };

        if docs.is_empty() {
            return Ok(());
        }

        let seg_id = {
            let mut segs = self.segments.write().unwrap();
            segs.next_segment_id()
        };

        let seg_dir = self.segments_dir().join(&seg_id);
        SegmentWriter::new(self.schema.clone())
            .write(&seg_id, &docs, &seg_dir)
            .map_err(search_core::Error::Io)?;

        let reader = search_index::SegmentReader::open(&seg_dir).map_err(search_core::Error::Io)?;
        let stats = search_query::SegmentStatistics::compute(&reader, &self.schema);
        let entry = SegmentEntry { segment_id: seg_id, dir: seg_dir, reader, stats };

        {
            let mut segs = self.segments.write().unwrap();
            segs.add_segment(entry);
        }

        // Truncate WAL after successful flush
        {
            let mut wal = self.wal.lock().unwrap();
            wal.truncate()?;
        }

        self.maybe_merge()?;
        Ok(())
    }

    fn maybe_merge(&self) -> search_core::Result<()> {
        let merge_groups: Vec<Vec<String>> = {
            let segs = self.segments.read().unwrap();
            self.merge_policy
                .find_merges(segs.segments())
                .into_iter()
                .map(|group| group.into_iter().map(|s| s.to_string()).collect())
                .collect()
        };

        for group in merge_groups {
            let refs: Vec<&str> = group.iter().map(String::as_str).collect();
            self.run_merge(&refs)?;
        }
        Ok(())
    }

    fn run_merge(&self, segment_ids: &[&str]) -> search_core::Result<()> {
        let new_seg_id = {
            let mut segs = self.segments.write().unwrap();
            segs.next_segment_id()
        };
        let new_dir = self.segments_dir().join(&new_seg_id);

        let new_entry = {
            let segs = self.segments.read().unwrap();
            let sources: Vec<&SegmentEntry> = segs
                .segments()
                .iter()
                .filter(|e| segment_ids.contains(&e.segment_id.as_str()))
                .collect();
            self.merge_scheduler.execute_merge(&sources, &new_seg_id, &new_dir)?
        };

        {
            let old_ids: Vec<String> = segment_ids.iter().map(|s| s.to_string()).collect();
            let mut segs = self.segments.write().unwrap();
            segs.replace_segments(&old_ids, new_entry);
        }

        // Clean up old segment dirs
        for old_id in segment_ids {
            let old_dir = self.segments_dir().join(old_id);
            let _ = std::fs::remove_dir_all(&old_dir);
        }

        Ok(())
    }

    fn segments_dir(&self) -> PathBuf {
        self.config.data_dir.join("segments")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{IndexSchema, ShardConfig};
    use std::collections::HashMap;

    fn make_config(data_dir: &std::path::Path) -> ShardConfig {
        ShardConfig {
            data_dir: data_dir.to_path_buf(),
            write_buffer_size: 1024 * 1024, // 1MB
            ..Default::default()
        }
    }

    fn make_doc(id: u64, title: &str, category: &str) -> Document {
        Document {
            id,
            title: title.into(),
            description: "a great product".into(),
            price: id as f64 * 100.0,
            category: category.into(),
            attributes: HashMap::new(),
        }
    }

    #[test]
    fn test_index_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema).unwrap();

        shard.index(make_doc(1, "Samsung Galaxy phone", "electronics")).unwrap();
        shard.index(make_doc(2, "Apple iPhone device", "electronics")).unwrap();
        shard.index(make_doc(3, "Nike running shoes", "clothing")).unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 1);
    }

    #[test]
    fn test_delete_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema).unwrap();

        shard.index(make_doc(1, "Samsung Galaxy", "electronics")).unwrap();
        shard.index(make_doc(2, "Samsung Note", "electronics")).unwrap();

        shard.delete(1).unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 2);
    }

    #[test]
    fn test_flush_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema).unwrap();

        for i in 1..=10u64 {
            shard.index(make_doc(i, "Samsung Galaxy product", "electronics")).unwrap();
        }

        shard.flush().unwrap();

        let stats = shard.stats();
        assert_eq!(stats.segment_count, 1);
        assert_eq!(stats.buffer_doc_count, 0);

        let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 10);
    }

    #[test]
    fn test_wal_recovery() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Index docs and flush them to WAL
        {
            let shard = ShardEngine::open(make_config(dir.path()), schema.clone()).unwrap();
            shard.index(make_doc(1, "Samsung Galaxy", "electronics")).unwrap();
            shard.index(make_doc(2, "Apple iPhone", "electronics")).unwrap();
            // Do NOT flush — docs are only in WAL + write buffer
        }

        // Re-open simulates a crash recovery (WAL is replayed)
        {
            let shard = ShardEngine::open(make_config(dir.path()), schema).unwrap();
            let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
            let resp = shard.search(req).unwrap();
            assert_eq!(resp.hits.len(), 1);
            assert_eq!(resp.hits[0].id, 1);
        }
    }

    #[test]
    fn test_merge_triggered() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        // Tiny buffer: 1 byte, so every doc triggers a flush
        let mut config = make_config(dir.path());
        config.write_buffer_size = 1;
        let shard = ShardEngine::open(config, schema).unwrap();

        // Each index call flushes immediately → creates many segments
        for i in 1..=5u64 {
            shard.index(make_doc(i, "Samsung product", "electronics")).unwrap();
        }

        // After merge policy kicks in, segment count should be reduced
        let stats = shard.stats();
        assert!(stats.segment_count < 5, "merge should have reduced segment count");

        let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 5);
    }
}
