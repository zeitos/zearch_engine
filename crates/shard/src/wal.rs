use search_core::Document;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WalEntry {
    Index(Document),
    Delete(u64),
}

pub struct WriteAheadLog {
    path: std::path::PathBuf,
    file: File,
}

impl WriteAheadLog {
    pub fn open(path: &Path) -> search_core::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(search_core::Error::Io)?;
        Ok(Self { path: path.to_path_buf(), file })
    }

    /// Append an entry and fsync.
    pub fn append(&mut self, entry: &WalEntry) -> search_core::Result<()> {
        let encoded = bincode::serialize(entry)
            .map_err(|e| search_core::Error::Wal(e.to_string()))?;
        let len = encoded.len() as u32;
        self.file.write_all(&len.to_le_bytes()).map_err(search_core::Error::Io)?;
        self.file.write_all(&encoded).map_err(search_core::Error::Io)?;
        self.file.flush().map_err(search_core::Error::Io)?;
        self.file.sync_data().map_err(search_core::Error::Io)?;
        Ok(())
    }

    /// Read all valid entries. Partial trailing entries are silently discarded.
    pub fn read_all(&mut self) -> search_core::Result<Vec<WalEntry>> {
        self.file.seek(SeekFrom::Start(0)).map_err(search_core::Error::Io)?;
        let mut reader = BufReader::new(&self.file);
        let mut entries = Vec::new();
        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(search_core::Error::Io(e)),
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            match reader.read_exact(&mut data) {
                Ok(()) => {}
                Err(_) => break, // partial entry — discard
            }
            match bincode::deserialize::<WalEntry>(&data) {
                Ok(entry) => entries.push(entry),
                Err(_) => break, // corrupted entry — stop here
            }
        }
        Ok(entries)
    }

    /// Truncate the WAL (called after a successful flush).
    pub fn truncate(&mut self) -> search_core::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(search_core::Error::Io)?;
        file.sync_all().map_err(search_core::Error::Io)?;
        // Re-open in append mode
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)
            .map_err(search_core::Error::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::Document;
    use std::collections::HashMap;

    fn make_doc(id: u64) -> Document {
        Document {
            id,
            title: format!("Doc {id}"),
            description: "test".into(),
            price: id as f64,
            category: "test".into(),
            attributes: HashMap::new(),
        }
    }

    #[test]
    fn test_append_and_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.wal");
        let mut wal = WriteAheadLog::open(&path).unwrap();

        wal.append(&WalEntry::Index(make_doc(1))).unwrap();
        wal.append(&WalEntry::Delete(42)).unwrap();
        wal.append(&WalEntry::Index(make_doc(2))).unwrap();

        let entries = wal.read_all().unwrap();
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], WalEntry::Index(d) if d.id == 1));
        assert!(matches!(&entries[1], WalEntry::Delete(42)));
        assert!(matches!(&entries[2], WalEntry::Index(d) if d.id == 2));
    }

    #[test]
    fn test_truncate() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.wal");
        let mut wal = WriteAheadLog::open(&path).unwrap();

        wal.append(&WalEntry::Index(make_doc(1))).unwrap();
        wal.truncate().unwrap();

        let entries = wal.read_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_empty_wal() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.wal");
        let mut wal = WriteAheadLog::open(&path).unwrap();

        let entries = wal.read_all().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_persist_and_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.wal");

        {
            let mut wal = WriteAheadLog::open(&path).unwrap();
            wal.append(&WalEntry::Index(make_doc(10))).unwrap();
            wal.append(&WalEntry::Delete(99)).unwrap();
        }

        // Reopen simulates restart
        let mut wal = WriteAheadLog::open(&path).unwrap();
        let entries = wal.read_all().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0], WalEntry::Index(d) if d.id == 10));
        assert!(matches!(&entries[1], WalEntry::Delete(99)));
    }
}
