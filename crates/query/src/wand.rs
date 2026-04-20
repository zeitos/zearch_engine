/// Block-Max WAND (BMW) conjunctive top-K retrieval.
///
/// Instead of scoring every matching document, WAND uses per-block upper-bound
/// scores to skip entire blocks of postings that cannot enter the top-K result
/// set. For a query with k=50 over a posting list of 5M entries, BMW typically
/// evaluates fewer than 1% of postings.
///
/// This implementation is CONJUNCTIVE (AND semantics): only documents appearing
/// in ALL per-token posting lists are candidates. The loop advances all cursors
/// in sync, using block upper bounds to jump over non-competitive blocks.
use crate::scorer::Bm25Scorer;
use search_index::posting::{PostingBlock, PostingList, BLOCK_SIZE};
use std::collections::BinaryHeap;
use std::cmp::Reverse;

/// A cursor over one (merged) posting list for a single query token.
pub struct TermCursor {
    postings: Vec<search_index::posting::Posting>,
    blocks: Vec<PostingBlock>,
    pub position: usize,
    /// Pre-computed IDF for this term cluster.
    pub idf: f32,
    pub avgdl: f32,
    scorer: Bm25Scorer,
}

impl TermCursor {
    pub fn new(mut pl: PostingList, idf: f32, avgdl: f32, scorer: Bm25Scorer) -> Self {
        // If blocks were not built during merge, rebuild them now.
        if pl.blocks.is_empty() && !pl.postings.is_empty() {
            pl.rebuild_blocks(avgdl);
        }
        Self {
            postings: pl.postings,
            blocks: pl.blocks,
            position: 0,
            idf,
            avgdl,
            scorer,
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.position >= self.postings.len()
    }

    pub fn current_doc_id(&self) -> u32 {
        self.postings[self.position].doc_id
    }

    /// Conservative upper bound on the exact BM25 score for any doc in the
    /// current block. Multiplied by 1.5 to account for the title-position boost
    /// that the executor may apply. Padded with (k1+1) for any description
    /// contribution (desc_boost=1.0, max_impact = k1+1 = 2.2).
    pub fn block_upper_bound(&self, title_boost: f32, desc_boost: f32) -> f32 {
        let block_idx = self.position / BLOCK_SIZE;
        let max_impact = self
            .blocks
            .get(block_idx)
            .map(|b| b.max_impact)
            .unwrap_or(0.0);
        // 1.5 × (title_boost × block_impact + desc_boost × (k1+1))
        1.5 * self.idf * (title_boost * max_impact + desc_boost * (crate::scorer::K1 + 1.0))
    }

    /// Approximate per-doc score from posting data alone (no doc fetch needed).
    pub fn score_current(&self) -> f32 {
        let p = &self.postings[self.position];
        self.idf * self.scorer.term_score(p.term_freq as u32, p.doc_len as u32, self.avgdl, 1.0)
    }

    /// Advance position to the first posting with doc_id >= `target`.
    /// Uses binary search for O(log N) skipping.
    pub fn advance_to(&mut self, target: u32) {
        let slice = &self.postings[self.position..];
        let jump = slice.partition_point(|p| p.doc_id < target);
        self.position += jump;
    }

    /// First doc_id of the next block (None if we're in the last block).
    pub fn next_block_doc_id(&self) -> Option<u32> {
        let next_pos = ((self.position / BLOCK_SIZE) + 1) * BLOCK_SIZE;
        self.postings.get(next_pos).map(|p| p.doc_id)
    }

    pub fn advance_one(&mut self) {
        self.position += 1;
    }
}

/// Min-heap of (score, doc_id) pairs for tracking the running top-K threshold.
pub struct TopKHeap {
    k: usize,
    heap: BinaryHeap<Reverse<(u32, u32)>>, // (score_bits, doc_id) — f32 bits for Ord
}

impl TopKHeap {
    pub fn new(k: usize) -> Self {
        Self { k, heap: BinaryHeap::with_capacity(k + 1) }
    }

    pub fn push(&mut self, doc_id: u32, score: f32) {
        self.heap.push(Reverse((score.to_bits(), doc_id)));
        if self.heap.len() > self.k {
            self.heap.pop();
        }
    }

    /// Current k-th best score (threshold for pruning). Returns 0 until heap is full.
    pub fn threshold(&self) -> f32 {
        if self.heap.len() < self.k {
            0.0
        } else {
            self.heap.peek().map(|Reverse((bits, _))| f32::from_bits(*bits)).unwrap_or(0.0)
        }
    }

    pub fn into_vec(self) -> Vec<(u32, f32)> {
        let mut v: Vec<(u32, f32)> = self
            .heap
            .into_iter()
            .map(|Reverse((bits, doc_id))| (doc_id, f32::from_bits(bits)))
            .collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v
    }
}

/// Conjunctive Block-Max WAND top-K retrieval.
///
/// `score_fn` receives the doc_id of each evaluated candidate and returns its
/// exact score (including per-field BM25 and any boosts). The function is
/// called only for candidates that survive the block-max upper-bound filter —
/// typically <1% of the total posting list for common queries.
///
/// Returns up to `k` (doc_id, score) pairs, sorted by score descending.
pub fn wand_top_k<F>(
    cursors: &mut Vec<TermCursor>,
    k: usize,
    title_boost: f32,
    desc_boost: f32,
    mut score_fn: F,
) -> Vec<(u32, f32)>
where
    F: FnMut(u32) -> Option<f32>,
{
    if cursors.is_empty() || k == 0 {
        return vec![];
    }

    let mut heap = TopKHeap::new(k);

    loop {
        // ── Step 1: find the highest doc_id across all cursors (the "pivot") ──
        let mut pivot_doc_id = 0u32;
        for c in cursors.iter() {
            if c.is_exhausted() {
                return heap.into_vec();
            }
            pivot_doc_id = pivot_doc_id.max(c.current_doc_id());
        }

        // ── Step 2: advance all cursors to the pivot ──
        for c in cursors.iter_mut() {
            c.advance_to(pivot_doc_id);
            if c.is_exhausted() {
                return heap.into_vec();
            }
        }

        // ── Step 3: check whether all cursors agree (AND condition) ──
        let all_agree = cursors.iter().all(|c| c.current_doc_id() == pivot_doc_id);
        if !all_agree {
            // At least one cursor jumped past pivot_doc_id; retry.
            continue;
        }

        // ── Step 4: Block-Max WAND filter ──
        let threshold = heap.threshold();
        let total_block_ub: f32 = cursors
            .iter()
            .map(|c| c.block_upper_bound(title_boost, desc_boost))
            .sum();

        if total_block_ub <= threshold {
            // No doc in these blocks can beat the threshold.
            // Jump to the earliest next-block boundary across all cursors.
            let next_doc = cursors
                .iter()
                .filter_map(|c| c.next_block_doc_id())
                .min();

            match next_doc {
                Some(nd) => {
                    for c in cursors.iter_mut() {
                        c.advance_to(nd);
                    }
                }
                None => return heap.into_vec(), // all cursors in their last block
            }
            continue;
        }

        // ── Step 5: evaluate candidate ──
        if let Some(score) = score_fn(pivot_doc_id) {
            if score > threshold || heap.threshold() == 0.0 {
                heap.push(pivot_doc_id, score);
            }
        }

        // ── Step 6: advance all cursors past this doc ──
        let next = pivot_doc_id + 1;
        for c in cursors.iter_mut() {
            c.advance_to(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scorer::Bm25Scorer;

    fn make_cursor(doc_ids: &[u32], avgdl: f32, idf: f32) -> TermCursor {
        let mut pl = PostingList::new();
        for &id in doc_ids {
            pl.add(id, 1, 8);
        }
        pl.rebuild_blocks(avgdl);
        TermCursor::new(pl, idf, avgdl, Bm25Scorer::default())
    }

    #[test]
    fn test_wand_single_term() {
        let mut cursors = vec![make_cursor(&[0, 1, 2, 3, 4], 8.0, 2.0)];
        let results = wand_top_k(&mut cursors, 3, 2.0, 1.0, |doc_id| Some(doc_id as f32));
        assert_eq!(results.len(), 3);
        // Should return the 3 highest-scored docs (4, 3, 2)
        assert_eq!(results[0].0, 4);
    }

    #[test]
    fn test_wand_and_semantics() {
        // Two terms: only docs in both lists should be returned
        let mut cursors = vec![
            make_cursor(&[0, 1, 2, 3], 8.0, 2.0),
            make_cursor(&[1, 3, 5], 8.0, 3.0),
        ];
        let mut evaluated = vec![];
        wand_top_k(&mut cursors, 10, 2.0, 1.0, |doc_id| {
            evaluated.push(doc_id);
            Some(1.0)
        });
        // Only docs 1 and 3 appear in both lists
        evaluated.sort();
        assert_eq!(evaluated, vec![1, 3]);
    }

    #[test]
    fn test_wand_empty_result_when_no_intersection() {
        let mut cursors = vec![
            make_cursor(&[0, 2, 4], 8.0, 1.0),
            make_cursor(&[1, 3, 5], 8.0, 1.0),
        ];
        let results = wand_top_k(&mut cursors, 10, 2.0, 1.0, |_| Some(1.0));
        assert!(results.is_empty());
    }
}
