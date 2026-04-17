use roaring::RoaringBitmap;
use search_core::Document;
use std::io;
use std::path::Path;

/// Writes documents to a store file with an offset index for random access.
pub struct DocStoreWriter {
    docs: Vec<Document>,
}

impl DocStoreWriter {
    pub fn new() -> Self {
        Self { docs: Vec::new() }
    }

    /// Add a document. The local index is its position (0-based).
    pub fn add(&mut self, doc: Document) {
        self.docs.push(doc);
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    /// Write the doc store to disk.
    /// Format: [offset_table_len: u32] [offset_table: u64 * N] [doc_data...]
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;

        let mut doc_data = Vec::new();
        let mut offsets = Vec::new();

        for doc in &self.docs {
            offsets.push(doc_data.len() as u64);
            let encoded = bincode::serialize(doc)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            doc_data.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            doc_data.extend_from_slice(&encoded);
        }

        let mut file_data = Vec::new();
        // Write number of docs
        file_data.extend_from_slice(&(self.docs.len() as u32).to_le_bytes());
        // Write offset table
        for offset in &offsets {
            file_data.extend_from_slice(&offset.to_le_bytes());
        }
        // Write doc data
        file_data.extend_from_slice(&doc_data);

        std::fs::write(dir.join("docs.bin"), file_data)
    }
}

impl Default for DocStoreWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads documents from a stored doc file.
pub struct DocStoreReader {
    data: Vec<u8>,
    doc_count: u32,
    offset_table_start: usize,
    data_start: usize,
    deletion_bitmap: RoaringBitmap,
}

impl DocStoreReader {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let data = std::fs::read(dir.join("docs.bin"))?;

        let doc_count = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let offset_table_start = 4;
        let data_start = offset_table_start + (doc_count as usize) * 8;

        // Load deletion bitmap if it exists
        let deletion_bitmap = Self::load_deletion_bitmap(dir)?;

        Ok(Self {
            data,
            doc_count,
            offset_table_start,
            data_start,
            deletion_bitmap,
        })
    }

    fn load_deletion_bitmap(dir: &Path) -> io::Result<RoaringBitmap> {
        let path = dir.join("deletions.bin");
        if path.exists() {
            let data = std::fs::read(&path)?;
            RoaringBitmap::deserialize_from(&data[..])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        } else {
            Ok(RoaringBitmap::new())
        }
    }

    /// Get a document by its local doc_id. Returns None if deleted or out of range.
    pub fn get(&self, local_doc_id: u32) -> Option<Document> {
        if local_doc_id >= self.doc_count || self.deletion_bitmap.contains(local_doc_id) {
            return None;
        }

        let offset_pos = self.offset_table_start + (local_doc_id as usize) * 8;
        let relative_offset =
            u64::from_le_bytes(self.data[offset_pos..offset_pos + 8].try_into().unwrap())
                as usize;
        let abs_offset = self.data_start + relative_offset;

        let doc_len =
            u32::from_le_bytes(self.data[abs_offset..abs_offset + 4].try_into().unwrap()) as usize;
        let doc_data = &self.data[abs_offset + 4..abs_offset + 4 + doc_len];

        bincode::deserialize(doc_data).ok()
    }

    pub fn doc_count(&self) -> u32 {
        self.doc_count
    }

    pub fn live_doc_count(&self) -> u32 {
        self.doc_count - self.deletion_bitmap.len() as u32
    }

    pub fn is_deleted(&self, local_doc_id: u32) -> bool {
        self.deletion_bitmap.contains(local_doc_id)
    }

    pub fn deletion_bitmap(&self) -> &RoaringBitmap {
        &self.deletion_bitmap
    }
}

/// Manages the deletion bitmap for a segment.
pub struct DeletionBitmap;

impl DeletionBitmap {
    /// Save a deletion bitmap to disk.
    pub fn save(bitmap: &RoaringBitmap, dir: &Path) -> io::Result<()> {
        let mut data = Vec::new();
        bitmap
            .serialize_into(&mut data)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        std::fs::write(dir.join("deletions.bin"), data)
    }

    /// Mark a document as deleted in the bitmap and save.
    pub fn mark_deleted(dir: &Path, local_doc_id: u32) -> io::Result<RoaringBitmap> {
        let path = dir.join("deletions.bin");
        let mut bitmap = if path.exists() {
            let data = std::fs::read(&path)?;
            RoaringBitmap::deserialize_from(&data[..])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        } else {
            RoaringBitmap::new()
        };
        bitmap.insert(local_doc_id);
        Self::save(&bitmap, dir)?;
        Ok(bitmap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_doc(id: u64, title: &str) -> Document {
        Document {
            id,
            title: title.to_string(),
            description: format!("Description for {title}"),
            price: id as f64 * 100.0,
            category: "test".to_string(),
            attributes: HashMap::new(),
        }
    }

    #[test]
    fn test_docstore_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();

        let mut writer = DocStoreWriter::new();
        writer.add(make_doc(1, "Samsung Galaxy"));
        writer.add(make_doc(2, "Apple iPhone"));
        writer.add(make_doc(3, "Google Pixel"));
        writer.write(dir.path()).unwrap();

        let reader = DocStoreReader::open(dir.path()).unwrap();
        assert_eq!(reader.doc_count(), 3);
        assert_eq!(reader.live_doc_count(), 3);

        let doc0 = reader.get(0).unwrap();
        assert_eq!(doc0.id, 1);
        assert_eq!(doc0.title, "Samsung Galaxy");

        let doc2 = reader.get(2).unwrap();
        assert_eq!(doc2.id, 3);
        assert_eq!(doc2.title, "Google Pixel");

        assert!(reader.get(3).is_none()); // out of range
    }

    #[test]
    fn test_deletion_bitmap() {
        let dir = tempfile::TempDir::new().unwrap();

        let mut writer = DocStoreWriter::new();
        writer.add(make_doc(1, "Doc A"));
        writer.add(make_doc(2, "Doc B"));
        writer.add(make_doc(3, "Doc C"));
        writer.write(dir.path()).unwrap();

        // Mark doc 1 (local_id=1) as deleted
        DeletionBitmap::mark_deleted(dir.path(), 1).unwrap();

        let reader = DocStoreReader::open(dir.path()).unwrap();
        assert_eq!(reader.doc_count(), 3);
        assert_eq!(reader.live_doc_count(), 2);

        assert!(reader.get(0).is_some());
        assert!(reader.get(1).is_none()); // deleted
        assert!(reader.get(2).is_some());
        assert!(reader.is_deleted(1));
    }

    #[test]
    fn test_empty_docstore() {
        let dir = tempfile::TempDir::new().unwrap();

        let writer = DocStoreWriter::new();
        writer.write(dir.path()).unwrap();

        let reader = DocStoreReader::open(dir.path()).unwrap();
        assert_eq!(reader.doc_count(), 0);
        assert!(reader.get(0).is_none());
    }
}
