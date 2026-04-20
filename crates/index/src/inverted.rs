use crate::posting::PostingList;
use fst::automaton::{Automaton, Levenshtein};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Writes an inverted index (FST term dict + posting lists) to disk.
pub struct InvertedIndexWriter {
    term_postings: BTreeMap<String, PostingList>,
}

impl InvertedIndexWriter {
    pub fn new() -> Self {
        Self { term_postings: BTreeMap::new() }
    }

    /// Add a term occurrence for a document.
    /// `doc_len` is the combined token count across all indexed fields for this document.
    pub fn add_term(&mut self, term: &str, doc_id: u32, doc_len: u16) {
        let pl = self.term_postings.entry(term.to_string()).or_default();
        if let Some(last) = pl.postings.last_mut() {
            if last.doc_id == doc_id {
                last.term_freq = last.term_freq.saturating_add(1);
                return;
            }
        }
        pl.add(doc_id, 1, doc_len);
    }

    /// Write the inverted index to disk.
    /// `avgdl` is the average combined doc length across the segment — used to compute
    /// block-max impact scores for Block-Max WAND.
    pub fn write(&mut self, dir: &Path, avgdl: f32) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;

        let fst_path = dir.join("inverted.fst");
        let post_path = dir.join("inverted.post");

        let mut posting_data = Vec::new();
        let mut fst_builder = MapBuilder::memory();

        for (term, pl) in &mut self.term_postings {
            pl.rebuild_blocks(avgdl);
            let offset = posting_data.len() as u64;
            let encoded = pl.encode();
            posting_data.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            posting_data.extend_from_slice(&encoded);

            fst_builder
                .insert(term.as_bytes(), offset)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        let fst_bytes = fst_builder
            .into_inner()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        std::fs::write(&fst_path, fst_bytes)?;
        std::fs::write(&post_path, posting_data)?;
        Ok(())
    }

    pub fn term_count(&self) -> usize {
        self.term_postings.len()
    }
}

impl Default for InvertedIndexWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads an inverted index from disk.
pub struct InvertedIndexReader {
    fst_map: Map<Vec<u8>>,
    posting_data: Vec<u8>,
}

impl InvertedIndexReader {
    pub fn open(dir: &Path) -> io::Result<Self> {
        let fst_bytes = std::fs::read(dir.join("inverted.fst"))?;
        let posting_data = std::fs::read(dir.join("inverted.post"))?;
        let fst_map =
            Map::new(fst_bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Self { fst_map, posting_data })
    }

    pub fn get_postings(&self, term: &str) -> Option<PostingList> {
        let offset = self.fst_map.get(term.as_bytes())? as usize;
        Some(self.read_at(offset))
    }

    pub fn fuzzy_search(&self, term: &str, max_distance: u32) -> Vec<(String, PostingList)> {
        let Ok(automaton) = Levenshtein::new(term, max_distance) else { return vec![] };
        let mut stream = self.fst_map.search(automaton).into_stream();
        let mut results = Vec::new();
        while let Some((key, offset)) = stream.next() {
            let t = String::from_utf8_lossy(key).to_string();
            results.push((t, self.read_at(offset as usize)));
        }
        results
    }

    pub fn prefix_search(&self, prefix: &str) -> Vec<(String, PostingList)> {
        let automaton = fst::automaton::Str::new(prefix).starts_with();
        let mut stream = self.fst_map.search(automaton).into_stream();
        let mut results = Vec::new();
        while let Some((key, offset)) = stream.next() {
            let t = String::from_utf8_lossy(key).to_string();
            results.push((t, self.read_at(offset as usize)));
        }
        results
    }

    pub fn term_count(&self) -> usize {
        self.fst_map.len()
    }

    fn read_at(&self, offset: usize) -> PostingList {
        let len =
            u32::from_le_bytes(self.posting_data[offset..offset + 4].try_into().unwrap()) as usize;
        PostingList::decode(&self.posting_data[offset + 4..offset + 4 + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_write_and_read() {
        let dir = TempDir::new().unwrap();
        let seg = dir.path().join("seg");

        let mut w = InvertedIndexWriter::new();
        w.add_term("samsung", 0, 5);
        w.add_term("galaxy", 0, 5);
        w.add_term("samsung", 1, 6);
        w.add_term("phone", 1, 6);
        w.add_term("samsung", 2, 4);
        w.add_term("galaxy", 2, 4);
        w.add_term("galaxy", 2, 4); // dup → tf=2

        w.write(&seg, 5.0).unwrap();

        let r = InvertedIndexReader::open(&seg).unwrap();
        assert_eq!(r.get_postings("samsung").unwrap().len(), 3);
        assert_eq!(r.get_postings("galaxy").unwrap().postings[1].term_freq, 2);
        assert_eq!(r.term_count(), 3);
    }

    #[test]
    fn test_blocks_written() {
        let dir = TempDir::new().unwrap();
        let seg = dir.path().join("seg");

        let mut w = InvertedIndexWriter::new();
        for i in 0..100u32 {
            w.add_term("common", i, 8);
        }
        w.write(&seg, 8.0).unwrap();

        let r = InvertedIndexReader::open(&seg).unwrap();
        let pl = r.get_postings("common").unwrap();
        assert_eq!(pl.len(), 100);
        assert_eq!(pl.blocks.len(), 2); // 64 + 36
        assert!(pl.blocks[0].max_impact > 0.0);
    }

    #[test]
    fn test_fuzzy_search() {
        let dir = TempDir::new().unwrap();
        let seg = dir.path().join("seg");

        let mut w = InvertedIndexWriter::new();
        w.add_term("samsung", 0, 5);
        w.add_term("samsnug", 1, 5);
        w.add_term("apple", 2, 4);
        w.write(&seg, 5.0).unwrap();

        let r = InvertedIndexReader::open(&seg).unwrap();
        let results = r.fuzzy_search("samsung", 2);
        let terms: Vec<&str> = results.iter().map(|(t, _)| t.as_str()).collect();
        assert!(terms.contains(&"samsung"));
        assert!(terms.contains(&"samsnug"));
    }

    #[test]
    fn test_prefix_search() {
        let dir = TempDir::new().unwrap();
        let seg = dir.path().join("seg");

        let mut w = InvertedIndexWriter::new();
        w.add_term("samsung", 0, 5);
        w.add_term("sandisk", 1, 5);
        w.add_term("apple", 2, 4);
        w.write(&seg, 5.0).unwrap();

        let r = InvertedIndexReader::open(&seg).unwrap();
        let results = r.prefix_search("sam");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "samsung");
    }
}
