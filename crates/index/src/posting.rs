use serde::{Deserialize, Serialize};

/// A single entry in a posting list.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Posting {
    pub doc_id: u32,
    pub term_freq: u32,
}

/// A posting list: sorted sequence of (doc_id, term_frequency) pairs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostingList {
    pub postings: Vec<Posting>,
}

impl PostingList {
    pub fn new() -> Self {
        Self {
            postings: Vec::new(),
        }
    }

    pub fn add(&mut self, doc_id: u32, term_freq: u32) {
        self.postings.push(Posting { doc_id, term_freq });
    }

    pub fn len(&self) -> usize {
        self.postings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.postings.is_empty()
    }

    /// Encode the posting list into a compressed byte representation.
    /// Uses delta encoding for doc IDs + simple varint encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Write count as varint
        encode_varint(self.postings.len() as u64, &mut buf);

        // Delta-encode doc IDs
        let mut prev_doc_id: u32 = 0;
        for posting in &self.postings {
            let delta = posting.doc_id - prev_doc_id;
            encode_varint(delta as u64, &mut buf);
            encode_varint(posting.term_freq as u64, &mut buf);
            prev_doc_id = posting.doc_id;
        }

        buf
    }

    /// Decode a posting list from compressed bytes.
    pub fn decode(data: &[u8]) -> Self {
        let mut offset = 0;

        let count = decode_varint(data, &mut offset) as usize;
        let mut postings = Vec::with_capacity(count);

        let mut prev_doc_id: u32 = 0;
        for _ in 0..count {
            let delta = decode_varint(data, &mut offset) as u32;
            let term_freq = decode_varint(data, &mut offset) as u32;
            let doc_id = prev_doc_id + delta;
            postings.push(Posting { doc_id, term_freq });
            prev_doc_id = doc_id;
        }

        Self { postings }
    }
}

/// Encode a u64 as a variable-length integer (LEB128).
fn encode_varint(mut value: u64, buf: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Decode a variable-length integer from bytes at the given offset.
fn decode_varint(data: &[u8], offset: &mut usize) -> u64 {
    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = data[*offset];
        *offset += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_posting_list_roundtrip() {
        let mut pl = PostingList::new();
        pl.add(5, 2);
        pl.add(10, 1);
        pl.add(100, 3);
        pl.add(105, 1);

        let encoded = pl.encode();
        let decoded = PostingList::decode(&encoded);

        assert_eq!(pl.postings, decoded.postings);
    }

    #[test]
    fn test_empty_posting_list() {
        let pl = PostingList::new();
        let encoded = pl.encode();
        let decoded = PostingList::decode(&encoded);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_single_posting() {
        let mut pl = PostingList::new();
        pl.add(42, 7);

        let encoded = pl.encode();
        let decoded = PostingList::decode(&encoded);

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded.postings[0].doc_id, 42);
        assert_eq!(decoded.postings[0].term_freq, 7);
    }

    #[test]
    fn test_large_doc_ids() {
        let mut pl = PostingList::new();
        pl.add(1_000_000, 1);
        pl.add(2_000_000, 2);
        pl.add(10_000_000, 1);

        let encoded = pl.encode();
        let decoded = PostingList::decode(&encoded);

        assert_eq!(pl.postings, decoded.postings);
    }

    #[test]
    fn test_delta_encoding_saves_space() {
        // Sequential doc IDs should compress well
        let mut sequential = PostingList::new();
        for i in 0..1000 {
            sequential.add(i, 1);
        }
        let seq_encoded = sequential.encode();

        // Sparse doc IDs compress less
        let mut sparse = PostingList::new();
        for i in 0..1000 {
            sparse.add(i * 10000, 1);
        }
        let sparse_encoded = sparse.encode();

        // Sequential should be smaller
        assert!(seq_encoded.len() < sparse_encoded.len());
    }

    #[test]
    fn test_varint_roundtrip() {
        let values = vec![0, 1, 127, 128, 255, 256, 16383, 16384, u64::MAX];
        for val in values {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            let mut offset = 0;
            let decoded = decode_varint(&buf, &mut offset);
            assert_eq!(val, decoded, "Failed for value {val}");
            assert_eq!(offset, buf.len());
        }
    }
}
