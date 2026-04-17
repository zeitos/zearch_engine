use crate::posting::PostingList;
use fst::automaton::{Automaton, Levenshtein};
use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Writes an inverted index (FST term dict + posting lists) to disk.
pub struct InvertedIndexWriter {
    /// term -> posting list (accumulated during indexing)
    term_postings: BTreeMap<String, PostingList>,
}

impl InvertedIndexWriter {
    pub fn new() -> Self {
        Self {
            term_postings: BTreeMap::new(),
        }
    }

    /// Add a term occurrence for a document.
    pub fn add_term(&mut self, term: &str, doc_id: u32) {
        let posting_list = self
            .term_postings
            .entry(term.to_string())
            .or_default();

        // If the last posting is for the same doc, increment frequency
        if let Some(last) = posting_list.postings.last_mut() {
            if last.doc_id == doc_id {
                last.term_freq += 1;
                return;
            }
        }
        posting_list.add(doc_id, 1);
    }

    /// Write the inverted index to disk.
    /// Creates two files: `inverted.fst` (term dict) and `inverted.post` (posting lists).
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;

        let fst_path = dir.join("inverted.fst");
        let post_path = dir.join("inverted.post");

        // Serialize all posting lists into a contiguous buffer, recording offsets
        let mut posting_data = Vec::new();
        let mut fst_builder = MapBuilder::memory();

        for (term, posting_list) in &self.term_postings {
            let offset = posting_data.len() as u64;
            let encoded = posting_list.encode();
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

    /// Number of unique terms.
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
    /// Open an inverted index from a segment directory.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let fst_bytes = std::fs::read(dir.join("inverted.fst"))?;
        let posting_data = std::fs::read(dir.join("inverted.post"))?;

        let fst_map =
            Map::new(fst_bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        Ok(Self {
            fst_map,
            posting_data,
        })
    }

    /// Exact term lookup — returns the posting list for a term if it exists.
    pub fn get_postings(&self, term: &str) -> Option<PostingList> {
        let offset = self.fst_map.get(term.as_bytes())? as usize;
        Some(self.read_posting_list_at(offset))
    }

    /// Fuzzy lookup — returns all terms within `max_distance` edit distance
    /// and their posting lists.
    pub fn fuzzy_search(&self, term: &str, max_distance: u32) -> Vec<(String, PostingList)> {
        let Ok(automaton) = Levenshtein::new(term, max_distance) else {
            return vec![];
        };

        let mut stream = self.fst_map.search(automaton).into_stream();
        let mut results = Vec::new();

        while let Some((key, offset)) = stream.next() {
            let term_str = String::from_utf8_lossy(key).to_string();
            let posting_list = self.read_posting_list_at(offset as usize);
            results.push((term_str, posting_list));
        }

        results
    }

    /// Prefix search — returns all terms starting with `prefix`.
    pub fn prefix_search(&self, prefix: &str) -> Vec<(String, PostingList)> {
        let automaton = fst::automaton::Str::new(prefix).starts_with();
        let mut stream = self.fst_map.search(automaton).into_stream();
        let mut results = Vec::new();

        while let Some((key, offset)) = stream.next() {
            let term_str = String::from_utf8_lossy(key).to_string();
            let posting_list = self.read_posting_list_at(offset as usize);
            results.push((term_str, posting_list));
        }

        results
    }

    /// Number of unique terms in the index.
    pub fn term_count(&self) -> usize {
        self.fst_map.len()
    }

    fn read_posting_list_at(&self, offset: usize) -> PostingList {
        let len =
            u32::from_le_bytes(self.posting_data[offset..offset + 4].try_into().unwrap()) as usize;
        let data = &self.posting_data[offset + 4..offset + 4 + len];
        PostingList::decode(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn test_write_and_read_inverted_index() {
        let dir = make_test_dir();
        let seg_dir = dir.path().join("seg");

        let mut writer = InvertedIndexWriter::new();
        writer.add_term("samsung", 0);
        writer.add_term("galaxy", 0);
        writer.add_term("samsung", 1);
        writer.add_term("phone", 1);
        writer.add_term("samsung", 2);
        writer.add_term("galaxy", 2);
        writer.add_term("galaxy", 2); // duplicate: should increment tf

        writer.write(&seg_dir).unwrap();

        let reader = InvertedIndexReader::open(&seg_dir).unwrap();

        // Exact lookups
        let samsung = reader.get_postings("samsung").unwrap();
        assert_eq!(samsung.len(), 3); // docs 0, 1, 2

        let galaxy = reader.get_postings("galaxy").unwrap();
        assert_eq!(galaxy.len(), 2); // docs 0, 2
        assert_eq!(galaxy.postings[1].term_freq, 2); // doc 2 has tf=2

        let phone = reader.get_postings("phone").unwrap();
        assert_eq!(phone.len(), 1);

        assert!(reader.get_postings("nonexistent").is_none());

        assert_eq!(reader.term_count(), 3);
    }

    #[test]
    fn test_fuzzy_search() {
        let dir = make_test_dir();
        let seg_dir = dir.path().join("seg");

        let mut writer = InvertedIndexWriter::new();
        writer.add_term("samsung", 0);
        writer.add_term("samsnug", 1); // typo
        writer.add_term("apple", 2);
        writer.add_term("laptop", 3);

        writer.write(&seg_dir).unwrap();

        let reader = InvertedIndexReader::open(&seg_dir).unwrap();

        // Search for "samsung" with distance 2 — should find "samsung" and "samsnug"
        // ("samsnug" is 2 edits from "samsung": u→n, n→u)
        let results = reader.fuzzy_search("samsung", 2);
        let terms: Vec<&str> = results.iter().map(|(t, _)| t.as_str()).collect();
        assert!(terms.contains(&"samsung"));
        assert!(terms.contains(&"samsnug"));
        assert!(!terms.contains(&"apple"));
    }

    #[test]
    fn test_prefix_search() {
        let dir = make_test_dir();
        let seg_dir = dir.path().join("seg");

        let mut writer = InvertedIndexWriter::new();
        writer.add_term("samsung", 0);
        writer.add_term("sandisk", 1);
        writer.add_term("apple", 2);
        writer.add_term("sapato", 3);

        writer.write(&seg_dir).unwrap();

        let reader = InvertedIndexReader::open(&seg_dir).unwrap();

        let results = reader.prefix_search("sam");
        let terms: Vec<&str> = results.iter().map(|(t, _)| t.as_str()).collect();
        assert!(terms.contains(&"samsung"));
        assert!(!terms.contains(&"sandisk")); // "san" != "sam"
        assert!(!terms.contains(&"apple"));

        let results = reader.prefix_search("sa");
        assert_eq!(results.len(), 3); // samsung, sandisk, sapato
    }

    #[test]
    fn test_empty_index() {
        let dir = make_test_dir();
        let seg_dir = dir.path().join("seg");

        let writer = InvertedIndexWriter::new();
        writer.write(&seg_dir).unwrap();

        let reader = InvertedIndexReader::open(&seg_dir).unwrap();
        assert_eq!(reader.term_count(), 0);
        assert!(reader.get_postings("anything").is_none());
    }
}
