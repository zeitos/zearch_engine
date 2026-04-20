use serde::{Deserialize, Serialize};

pub const BLOCK_SIZE: usize = 64;
pub const K1: f32 = 1.2;
pub const B: f32 = 0.75;

/// A single entry in a posting list.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Posting {
    pub doc_id: u32,
    /// Term frequency in the document across all indexed fields.
    pub term_freq: u8,
    /// Combined token count across all indexed fields for this document.
    pub doc_len: u16,
}

/// Per-block precomputed upper bound on the BM25 impact score (without IDF × field boost).
/// Enables Block-Max WAND to skip entire blocks of postings that cannot enter top-K.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PostingBlock {
    /// max over docs in block of: tf×(k1+1) / (tf + k1×(1−b+b×dl/avgdl))
    pub max_impact: f32,
}

/// A posting list: sorted sequence of postings (by doc_id), divided into fixed-size blocks.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PostingList {
    pub postings: Vec<Posting>,
    /// One block header per BLOCK_SIZE postings.
    pub blocks: Vec<PostingBlock>,
}

/// BM25 per-doc impact score for a single term occurrence, without IDF or field boost.
#[inline]
pub fn bm25_impact(tf: u8, doc_len: u16, avgdl: f32) -> f32 {
    let tf = tf as f32;
    let dl = doc_len as f32;
    let avgdl = avgdl.max(1.0);
    let num = tf * (K1 + 1.0);
    let denom = tf + K1 * (1.0 - B + B * dl / avgdl);
    num / denom
}

impl PostingList {
    pub fn new() -> Self {
        Self { postings: Vec::new(), blocks: Vec::new() }
    }

    pub fn add(&mut self, doc_id: u32, term_freq: u8, doc_len: u16) {
        self.postings.push(Posting { doc_id, term_freq, doc_len });
    }

    pub fn len(&self) -> usize {
        self.postings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.postings.is_empty()
    }

    /// Recompute block headers from current postings and segment avgdl.
    pub fn rebuild_blocks(&mut self, avgdl: f32) {
        let num_blocks = self.postings.len().div_ceil(BLOCK_SIZE);
        self.blocks = (0..num_blocks)
            .map(|b| {
                let start = b * BLOCK_SIZE;
                let end = (start + BLOCK_SIZE).min(self.postings.len());
                let max_impact = self.postings[start..end]
                    .iter()
                    .map(|p| bm25_impact(p.term_freq, p.doc_len, avgdl))
                    .fold(0.0f32, f32::max);
                PostingBlock { max_impact }
            })
            .collect();
    }

    /// Merge multiple posting lists into one sorted list.
    /// For duplicate doc_ids (prefix expansion), TFs are summed; doc_len is taken from the first.
    pub fn merge(lists: impl IntoIterator<Item = PostingList>) -> Self {
        let mut merged: Vec<Posting> = lists
            .into_iter()
            .flat_map(|pl| pl.postings.into_iter())
            .collect();
        merged.sort_unstable_by_key(|p| p.doc_id);

        let mut postings: Vec<Posting> = Vec::with_capacity(merged.len());
        for p in merged {
            if let Some(last) = postings.last_mut() {
                if last.doc_id == p.doc_id {
                    last.term_freq = last.term_freq.saturating_add(p.term_freq);
                    continue;
                }
            }
            postings.push(p);
        }
        // blocks must be rebuilt with correct avgdl by the caller
        Self { postings, blocks: Vec::new() }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        encode_varint(self.blocks.len() as u64, &mut buf);
        for block in &self.blocks {
            buf.extend_from_slice(&block.max_impact.to_le_bytes());
        }

        encode_varint(self.postings.len() as u64, &mut buf);
        let mut prev = 0u32;
        for p in &self.postings {
            encode_varint((p.doc_id - prev) as u64, &mut buf);
            encode_varint(p.term_freq as u64, &mut buf);
            encode_varint(p.doc_len as u64, &mut buf);
            prev = p.doc_id;
        }
        buf
    }

    pub fn decode(data: &[u8]) -> Self {
        let mut offset = 0;

        let num_blocks = decode_varint(data, &mut offset) as usize;
        let mut blocks = Vec::with_capacity(num_blocks);
        for _ in 0..num_blocks {
            let impact = f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
            blocks.push(PostingBlock { max_impact: impact });
        }

        let count = decode_varint(data, &mut offset) as usize;
        let mut postings = Vec::with_capacity(count);
        let mut prev = 0u32;
        for _ in 0..count {
            let delta = decode_varint(data, &mut offset) as u32;
            let tf = decode_varint(data, &mut offset) as u8;
            let dl = decode_varint(data, &mut offset) as u16;
            let doc_id = prev + delta;
            postings.push(Posting { doc_id, term_freq: tf, doc_len: dl });
            prev = doc_id;
        }
        Self { postings, blocks }
    }
}

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
        pl.add(5, 2, 10);
        pl.add(10, 1, 8);
        pl.add(100, 3, 12);
        pl.add(105, 1, 6);
        pl.rebuild_blocks(9.0);

        let encoded = pl.encode();
        let decoded = PostingList::decode(&encoded);

        assert_eq!(pl.postings, decoded.postings);
        assert_eq!(pl.blocks.len(), decoded.blocks.len());
    }

    #[test]
    fn test_empty_posting_list() {
        let mut pl = PostingList::new();
        pl.rebuild_blocks(1.0);
        let encoded = pl.encode();
        let decoded = PostingList::decode(&encoded);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_block_max_scores() {
        let mut pl = PostingList::new();
        // Short doc should have higher impact
        pl.add(0, 1, 3);   // short doc
        pl.add(1, 1, 20);  // long doc
        pl.rebuild_blocks(10.0);

        assert_eq!(pl.blocks.len(), 1);
        let short_impact = bm25_impact(1, 3, 10.0);
        let long_impact = bm25_impact(1, 20, 10.0);
        assert!(short_impact > long_impact);
        assert!((pl.blocks[0].max_impact - short_impact).abs() < 1e-6);
    }

    #[test]
    fn test_multiple_blocks() {
        let mut pl = PostingList::new();
        for i in 0..130u32 {
            pl.add(i, 1, 8);
        }
        pl.rebuild_blocks(8.0);
        assert_eq!(pl.blocks.len(), 3); // 64 + 64 + 2
    }

    #[test]
    fn test_merge_deduplicates() {
        let mut pl1 = PostingList::new();
        pl1.add(1, 1, 5);
        pl1.add(3, 1, 5);

        let mut pl2 = PostingList::new();
        pl2.add(2, 1, 5);
        pl2.add(3, 2, 5); // same doc_id, tf should sum

        let mut merged = PostingList::merge([pl1, pl2]);
        merged.rebuild_blocks(5.0);

        assert_eq!(merged.len(), 3);
        assert_eq!(merged.postings[2].doc_id, 3);
        assert_eq!(merged.postings[2].term_freq, 3); // 1 + 2
    }

    #[test]
    fn test_varint_roundtrip() {
        for val in [0u64, 1, 127, 128, 255, 256, 16383, 16384, u32::MAX as u64] {
            let mut buf = Vec::new();
            encode_varint(val, &mut buf);
            let mut offset = 0;
            assert_eq!(decode_varint(&buf, &mut offset), val);
        }
    }
}
