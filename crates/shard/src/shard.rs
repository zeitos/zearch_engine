use crate::merge::{MergeScheduler, TieredMergePolicy};
use crate::replication::ReplicationManager;
use crate::segment_list::{SegmentEntry, SegmentList};
use crate::wal::{WalEntry, WriteAheadLog};
use crate::write_buffer::WriteBuffer;
use search_core::{Document, IndexSchema, SearchRequest, SearchResponse, ShardConfig, ShardRole};
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

pub struct ReindexStats {
    pub shard_id: u32,
    pub docs_reindexed: u64,
    pub elapsed_ms: u64,
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
    replication: Option<ReplicationManager>,
}

impl ShardEngine {
    /// Open or create a shard. Replays WAL if entries exist.
    /// Async because replicas may need to catch up from the primary on startup.
    pub async fn open(config: ShardConfig, schema: IndexSchema) -> search_core::Result<Self> {
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

        // Start replication manager if this is a primary with configured replicas.
        let replication = if config.role == ShardRole::Primary && !config.replica_endpoints.is_empty() {
            Some(ReplicationManager::start(config.replica_endpoints.clone()))
        } else {
            None
        };

        let engine = Self {
            config,
            schema,
            wal: Mutex::new(wal),
            write_buffer: std::sync::RwLock::new(buffer),
            segments: std::sync::RwLock::new(segments),
            searcher,
            merge_scheduler,
            merge_policy: TieredMergePolicy::default(),
            replication,
        };

        // If this is a replica, catch up from the primary before serving.
        if engine.config.role == ShardRole::Replica {
            if let Some(ref primary) = engine.config.primary_endpoint.clone() {
                if let Err(e) = engine.catch_up_from_primary(primary).await {
                    tracing::warn!(err = %e, "replica catch-up failed, starting empty");
                }
            }
        }

        Ok(engine)
    }

    /// Index a document: append to WAL, add to write buffer, flush if full.
    pub fn index(&self, doc: Document) -> search_core::Result<()> {
        let entry = WalEntry::Index(doc.clone());
        let seq = {
            let mut wal = self.wal.lock().unwrap();
            wal.append(&entry)?
        };
        if let Some(ref rm) = self.replication {
            rm.enqueue(seq, entry);
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
        let seqs = {
            let mut wal = self.wal.lock().unwrap();
            wal.append_batch(&wal_entries)?
        };
        if let Some(ref rm) = self.replication {
            for (entry, seq) in wal_entries.iter().zip(seqs.iter()) {
                rm.enqueue(*seq, entry.clone());
            }
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
        let entry = WalEntry::Delete(doc_id);
        let seq = {
            let mut wal = self.wal.lock().unwrap();
            wal.append(&entry)?
        };
        if let Some(ref rm) = self.replication {
            rm.enqueue(seq, entry);
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

    /// Rebuild the entire index from stored documents using the current schema.
    /// Reads all live docs, writes one new segment, atomically replaces old segments.
    pub fn reindex_self(&self) -> search_core::Result<ReindexStats> {
        let t0 = std::time::Instant::now();

        // Step 1: flush write buffer so all docs are in segments
        self.flush()?;

        // Step 2: collect all live documents from all segments
        let docs: Vec<search_core::Document> = {
            let segs = self.segments.read().unwrap();
            let mut collected = Vec::new();
            for entry in segs.segments() {
                for local_id in entry.reader.all_live_docs() {
                    if let Some(doc) = entry.reader.get_doc(local_id) {
                        collected.push(doc);
                    }
                }
            }
            collected
        };

        let docs_reindexed = docs.len() as u64;

        if docs.is_empty() {
            return Ok(ReindexStats {
                shard_id: self.config.shard_id,
                docs_reindexed: 0,
                elapsed_ms: t0.elapsed().as_millis() as u64,
            });
        }

        // Step 3: write new segment with current schema
        let new_seg_id = {
            let mut segs = self.segments.write().unwrap();
            segs.next_segment_id()
        };
        let new_dir = self.segments_dir().join(&new_seg_id);
        SegmentWriter::new(self.schema.clone())
            .write(&new_seg_id, &docs, &new_dir)
            .map_err(search_core::Error::Io)?;

        let reader = search_index::SegmentReader::open(&new_dir).map_err(search_core::Error::Io)?;
        let stats = search_query::SegmentStatistics::compute(&reader, &self.schema);
        let new_entry = SegmentEntry { segment_id: new_seg_id, dir: new_dir, reader, stats };

        // Step 4: atomic swap — replace all old segments with the new one
        let old_ids: Vec<String> = {
            let segs = self.segments.read().unwrap();
            segs.segments().iter().map(|e| e.segment_id.clone()).collect()
        };
        {
            let mut segs = self.segments.write().unwrap();
            segs.replace_segments(&old_ids, new_entry);
        }

        // Delete old segment directories from disk
        for old_id in &old_ids {
            let _ = std::fs::remove_dir_all(self.segments_dir().join(old_id));
        }

        // Step 5: truncate WAL (all docs are now in the new segment)
        {
            let mut wal = self.wal.lock().unwrap();
            wal.truncate()?;
        }

        Ok(ReindexStats {
            shard_id: self.config.shard_id,
            docs_reindexed,
            elapsed_ms: t0.elapsed().as_millis() as u64,
        })
    }

    /// Apply a replicated entry directly (no WAL — primary is the WAL source of truth).
    /// Called by the gRPC Replicate handler on replica nodes.
    pub fn apply_replicated(&self, entry: WalEntry) -> search_core::Result<()> {
        match entry {
            WalEntry::Index(doc) => {
                let full = {
                    let mut buf = self.write_buffer.write().unwrap();
                    buf.add(doc)
                };
                if full { self.flush()?; }
            }
            WalEntry::Delete(doc_id) => {
                let mut buf = self.write_buffer.write().unwrap();
                buf.mark_deleted(doc_id);
                drop(buf);
                let mut segs = self.segments.write().unwrap();
                segs.mark_deleted(doc_id)?;
            }
        }
        Ok(())
    }

    /// Return all live documents from segments + write buffer — used by primary for FullSync.
    pub fn get_all_live_docs(&self) -> Vec<search_core::Document> {
        let mut docs = Vec::new();
        // Segments first
        {
            let segs = self.segments.read().unwrap();
            for entry in segs.segments() {
                for local_id in entry.reader.all_live_docs() {
                    if let Some(doc) = entry.reader.get_doc(local_id) {
                        docs.push(doc);
                    }
                }
            }
        }
        // Write buffer
        {
            let buf = self.write_buffer.read().unwrap();
            docs.extend(buf.all_docs());
        }
        docs
    }

    /// Return WAL records since `from_seq` — used by the primary to serve catch-up requests.
    pub fn wal_records_since(&self, from_seq: u64) -> search_core::Result<Vec<crate::wal::WalRecord>> {
        let mut wal = self.wal.lock().unwrap();
        wal.read_since(from_seq)
    }

    pub fn is_replica(&self) -> bool {
        self.config.role == ShardRole::Replica
    }

    /// Catch up from the primary: first WAL catch-up, then full sync if needed.
    async fn catch_up_from_primary(&self, primary_endpoint: &str) -> search_core::Result<()> {
        use search_proto::shard::shard_service_client::ShardServiceClient;
        use search_proto::shard::{replicate_request, CatchUpRequest, FullSyncRequest};
        use tokio_stream::StreamExt;

        let from_seq = {
            let wal = self.wal.lock().unwrap();
            wal.next_seq()
        };

        tracing::info!(from_seq, primary = %primary_endpoint, "replica catch-up starting");

        let mut client = ShardServiceClient::connect(primary_endpoint.to_string())
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?;

        // Phase 1: WAL catch-up (new writes since last known seq)
        let mut stream = client
            .replicate_catch_up(tonic::Request::new(CatchUpRequest { from_wal_seq: from_seq }))
            .await
            .map_err(|e| search_core::Error::Grpc(e.to_string()))?
            .into_inner();

        let mut wal_count = 0u64;
        while let Some(req) = stream.next().await {
            let req = req.map_err(|e| search_core::Error::Grpc(e.to_string()))?;
            let entry = match req.operation {
                Some(replicate_request::Operation::IndexDoc(doc)) => WalEntry::Index(search_core::Document {
                    id: doc.id, title: doc.title, description: doc.description,
                    price: doc.price, category: doc.category,
                    attributes: doc.attributes.into_iter()
                        .map(|(k, v)| (k, search_core::Value::String(v))).collect(),
                }),
                Some(replicate_request::Operation::DeleteDocId(id)) => WalEntry::Delete(id),
                Some(replicate_request::Operation::Flush(_)) => { self.flush()?; continue; }
                None => continue,
            };
            self.apply_replicated(entry)?;
            wal_count += 1;
        }

        // Phase 2: full sync from segments if WAL had nothing and replica is empty
        let replica_has_data = {
            let segs = self.segments.read().unwrap();
            segs.segments().iter().any(|e| !e.reader.all_live_docs().is_empty())
        };

        if wal_count == 0 && !replica_has_data {
            tracing::info!(primary = %primary_endpoint, "WAL empty and replica has no data — starting full sync");
            let mut sync_stream = client
                .full_sync(tonic::Request::new(FullSyncRequest {}))
                .await
                .map_err(|e| search_core::Error::Grpc(e.to_string()))?
                .into_inner();

            let mut sync_count = 0u64;
            while let Some(proto) = sync_stream.next().await {
                let proto = proto.map_err(|e| search_core::Error::Grpc(e.to_string()))?;
                let doc = search_core::Document {
                    id: proto.id, title: proto.title, description: proto.description,
                    price: proto.price, category: proto.category,
                    attributes: proto.attributes.into_iter()
                        .map(|(k, v)| (k, search_core::Value::String(v))).collect(),
                };
                self.apply_replicated(WalEntry::Index(doc))?;
                sync_count += 1;
            }
            // Flush to segment so data survives replica restart
            self.flush()?;
            tracing::info!(sync_count, "full sync complete");
        } else {
            tracing::info!(wal_count, "WAL catch-up complete");
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

    #[tokio::test]
    async fn test_index_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema).await.unwrap();

        shard.index(make_doc(1, "Samsung Galaxy phone", "electronics")).unwrap();
        shard.index(make_doc(2, "Apple iPhone device", "electronics")).unwrap();
        shard.index(make_doc(3, "Nike running shoes", "clothing")).unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 1);
    }

    #[tokio::test]
    async fn test_delete_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema).await.unwrap();

        shard.index(make_doc(1, "Samsung Galaxy", "electronics")).unwrap();
        shard.index(make_doc(2, "Samsung Note", "electronics")).unwrap();

        shard.delete(1).unwrap();

        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 2);
    }

    #[tokio::test]
    async fn test_flush_and_search() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema).await.unwrap();

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

    #[tokio::test]
    async fn test_wal_recovery() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        // Index docs and flush them to WAL
        {
            let shard = ShardEngine::open(make_config(dir.path()), schema.clone()).await.unwrap();
            shard.index(make_doc(1, "Samsung Galaxy", "electronics")).unwrap();
            shard.index(make_doc(2, "Apple iPhone", "electronics")).unwrap();
            // Do NOT flush — docs are only in WAL + write buffer
        }

        // Re-open simulates a crash recovery (WAL is replayed)
        {
            let shard = ShardEngine::open(make_config(dir.path()), schema).await.unwrap();
            let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
            let resp = shard.search(req).unwrap();
            assert_eq!(resp.hits.len(), 1);
            assert_eq!(resp.hits[0].id, 1);
        }
    }

    #[tokio::test]
    async fn test_merge_triggered() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        // Tiny buffer: 1 byte, so every doc triggers a flush
        let mut config = make_config(dir.path());
        config.write_buffer_size = 1;
        let shard = ShardEngine::open(config, schema).await.unwrap();

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

    #[tokio::test]
    async fn test_reindex_self() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let shard = ShardEngine::open(make_config(dir.path()), schema.clone()).await.unwrap();

        for i in 1..=10u64 {
            shard.index(make_doc(i, "Samsung Galaxy product", "electronics")).unwrap();
        }
        shard.flush().unwrap();
        assert_eq!(shard.stats().segment_count, 1);

        let stats = shard.reindex_self().unwrap();
        assert_eq!(stats.docs_reindexed, 10);
        // Still one segment after reindex
        assert_eq!(shard.stats().segment_count, 1);

        // All docs still searchable
        let req = SearchRequest { query: "samsung".into(), limit: 20, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 10);
    }

    #[tokio::test]
    async fn test_reindex_deletes_old_segments_from_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let mut config = make_config(dir.path());
        config.write_buffer_size = 1; // force flush on every doc → multiple segments
        let shard = ShardEngine::open(config, schema).await.unwrap();

        for i in 1..=5u64 {
            shard.index(make_doc(i, "Samsung product", "electronics")).unwrap();
        }
        let before = shard.stats().segment_count;
        assert!(before >= 1);

        shard.reindex_self().unwrap();

        // After reindex: exactly 1 segment
        assert_eq!(shard.stats().segment_count, 1);

        // Old segment dirs should be gone
        let seg_dirs: Vec<_> = std::fs::read_dir(dir.path().join("segments"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        assert_eq!(seg_dirs.len(), 1, "only new segment dir should remain");
    }

    // -----------------------------------------------------------------------
    // Replication — unit tests (no gRPC)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_replication_manager_noop_on_empty() {
        use crate::replication::ReplicationManager;
        let rm = ReplicationManager::start(vec![]);
        assert_eq!(rm.replica_count(), 0);
        rm.enqueue(0, WalEntry::Delete(42)); // must not panic
    }

    #[tokio::test]
    async fn test_replication_manager_replica_count() {
        use crate::replication::ReplicationManager;
        // Unreachable endpoint — background task retries but start() returns immediately
        let rm = ReplicationManager::start(vec!["http://127.0.0.1:1".into()]);
        assert_eq!(rm.replica_count(), 1);
        rm.enqueue(1, WalEntry::Delete(1)); // must not block
    }

    #[tokio::test]
    async fn test_apply_replicated_does_not_write_wal() {
        use search_core::ShardRole;
        let dir = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();
        let mut config = make_config(dir.path());
        config.role = ShardRole::Replica;

        let shard = ShardEngine::open(config, schema).await.unwrap();
        shard.apply_replicated(WalEntry::Index(make_doc(1, "Samsung Galaxy", "electronics"))).unwrap();

        // Doc is searchable via the write buffer
        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };
        let resp = shard.search(req).unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].id, 1);

        // WAL must remain empty — replicas do NOT write to WAL
        let records = shard.wal_records_since(0).unwrap();
        assert!(records.is_empty(), "apply_replicated must not write to WAL");
    }

    #[tokio::test]
    async fn test_is_replica_flag() {
        use search_core::ShardRole;
        let dir1 = tempfile::TempDir::new().unwrap();
        let dir2 = tempfile::TempDir::new().unwrap();
        let schema = IndexSchema::default_product_schema();

        let primary = ShardEngine::open(make_config(dir1.path()), schema.clone()).await.unwrap();
        assert!(!primary.is_replica());

        let mut cfg = make_config(dir2.path());
        cfg.role = ShardRole::Replica;
        let replica = ShardEngine::open(cfg, schema).await.unwrap();
        assert!(replica.is_replica());
    }
}
