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

/// WAL entry with a monotonically increasing sequence number.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalRecord {
    pub seq: u64,
    pub entry: WalEntry,
}

pub struct WriteAheadLog {
    path: std::path::PathBuf,
    file: File,
    /// Next sequence number to assign.
    next_seq: u64,
}

impl WriteAheadLog {
    pub fn open(path: &Path) -> search_core::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(search_core::Error::Io)?;
        let mut wal = Self { path: path.to_path_buf(), file, next_seq: 0 };
        // Scan existing records to find the highest sequence number.
        let records = wal.read_records()?;
        if let Some(last) = records.last() {
            wal.next_seq = last.seq + 1;
        }
        Ok(wal)
    }

    /// Append an entry, assign a sequence number, fsync, and return the sequence number.
    pub fn append(&mut self, entry: &WalEntry) -> search_core::Result<u64> {
        let seqs = self.append_batch(std::slice::from_ref(entry))?;
        Ok(seqs[0])
    }

    /// Append multiple entries with a single fsync. Returns the assigned sequence numbers.
    pub fn append_batch(&mut self, entries: &[WalEntry]) -> search_core::Result<Vec<u64>> {
        let mut seqs = Vec::with_capacity(entries.len());
        for entry in entries {
            let seq = self.next_seq;
            self.next_seq += 1;
            let record = WalRecord { seq, entry: entry.clone() };
            let encoded = bincode::serialize(&record)
                .map_err(|e| search_core::Error::Wal(e.to_string()))?;
            let len = encoded.len() as u32;
            self.file.write_all(&len.to_le_bytes()).map_err(search_core::Error::Io)?;
            self.file.write_all(&encoded).map_err(search_core::Error::Io)?;
            seqs.push(seq);
        }
        self.file.flush().map_err(search_core::Error::Io)?;
        self.file.sync_data().map_err(search_core::Error::Io)?;
        Ok(seqs)
    }

    /// Read all valid records. Partial trailing records are silently discarded.
    pub fn read_records(&mut self) -> search_core::Result<Vec<WalRecord>> {
        self.file.seek(SeekFrom::Start(0)).map_err(search_core::Error::Io)?;
        let mut reader = BufReader::new(&self.file);
        let mut records = Vec::new();
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
                Err(_) => break,
            }
            match bincode::deserialize::<WalRecord>(&data) {
                Ok(record) => records.push(record),
                Err(_) => break,
            }
        }
        Ok(records)
    }

    /// Read all entries (without sequence numbers) — used for crash recovery.
    pub fn read_all(&mut self) -> search_core::Result<Vec<WalEntry>> {
        Ok(self.read_records()?.into_iter().map(|r| r.entry).collect())
    }

    /// Read all records with seq >= `from_seq` — used for replica catch-up.
    pub fn read_since(&mut self, from_seq: u64) -> search_core::Result<Vec<WalRecord>> {
        Ok(self.read_records()?.into_iter().filter(|r| r.seq >= from_seq).collect())
    }

    /// The next sequence number that will be assigned (= highest written seq + 1).
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Truncate the WAL (called after a successful flush).
    pub fn truncate(&mut self) -> search_core::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(search_core::Error::Io)?;
        file.sync_all().map_err(search_core::Error::Io)?;
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
