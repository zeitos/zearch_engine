use std::sync::Arc;

use crate::term::SuggestTerm;

/// Lossy, eventually-consistent suggest index.
/// Sorted lexicographically by term; built at flush time.
/// Terms held as `Arc<str>` so the builder and index can share string storage
/// across flushes without per-term reallocation.
#[derive(Debug, Default, Clone)]
pub struct SuggestIndex {
    terms: Vec<(Arc<str>, f32)>,
}

impl SuggestIndex {
    pub fn new(terms: Vec<(Arc<str>, f32)>) -> Self {
        let mut t = terms;
        t.sort_by(|a, b| a.0.cmp(&b.0));
        Self { terms: t }
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Prefix query. Case-insensitive. Returns up to `limit` terms.
    /// Scans prefix range in O(log N + M), picks top-`limit` in O(M) via
    /// quickselect, clones only the final `limit` strings.
    pub fn query(&self, prefix: &str, limit: usize) -> Vec<SuggestTerm> {
        use std::cmp::Ordering;

        if prefix.is_empty() || limit == 0 {
            return vec![];
        }
        let prefix_lower = prefix.to_lowercase();
        let start = self
            .terms
            .partition_point(|(t, _)| t.as_ref() < prefix_lower.as_str());

        // Collect indices of matching terms — cheap (usize only).
        let mut matches: Vec<usize> = self.terms[start..]
            .iter()
            .enumerate()
            .take_while(|(_, (t, _))| t.starts_with(prefix_lower.as_str()))
            .map(|(i, _)| start + i)
            .collect();

        if matches.is_empty() {
            return vec![];
        }

        let cmp_desc = |a: &usize, b: &usize| {
            self.terms[*b]
                .1
                .partial_cmp(&self.terms[*a].1)
                .unwrap_or(Ordering::Equal)
        };

        // Partition so the first `limit` elements are the top-limit (unordered).
        if matches.len() > limit {
            matches.select_nth_unstable_by(limit - 1, cmp_desc);
            matches.truncate(limit);
        }
        // Sort only the small top-limit slice by score desc.
        matches.sort_by(cmp_desc);

        matches
            .into_iter()
            .map(|i| SuggestTerm {
                term: self.terms[i].0.to_string(),
                score: self.terms[i].1,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(pairs: &[(&str, f32)]) -> SuggestIndex {
        SuggestIndex::new(pairs.iter().map(|(t, s)| (Arc::from(*t), *s)).collect())
    }

    #[test]
    fn prefix_match() {
        let i = idx(&[("wired", 0.6), ("wireless", 0.9), ("wood", 0.3)]);
        let r = i.query("wire", 5);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].term, "wireless");
        assert_eq!(r[1].term, "wired");
    }

    #[test]
    fn case_insensitive() {
        let i = idx(&[("wireless", 0.9)]);
        assert_eq!(i.query("WIRE", 5).len(), 1);
        assert_eq!(i.query("Wire", 5).len(), 1);
    }

    #[test]
    fn empty_prefix() {
        let i = idx(&[("wireless", 0.9)]);
        assert!(i.query("", 5).is_empty());
    }

    #[test]
    fn score_ordering() {
        let i = idx(&[("wire", 0.1), ("wired", 0.6), ("wireless", 0.9)]);
        let r = i.query("w", 3);
        assert_eq!(r[0].term, "wireless");
        assert_eq!(r[1].term, "wired");
        assert_eq!(r[2].term, "wire");
    }

    #[test]
    fn respects_limit() {
        let i = idx(&[("wa", 0.1), ("wb", 0.2), ("wc", 0.3)]);
        assert_eq!(i.query("w", 2).len(), 2);
    }

    #[test]
    fn no_match() {
        let i = idx(&[("wireless", 0.9)]);
        assert!(i.query("zz", 5).is_empty());
    }

    #[test]
    fn short_prefix_over_many_terms_is_fast() {
        // 10k matching terms with varying scores under prefix "w".
        let terms: Vec<(Arc<str>, f32)> = (0..10_000)
            .map(|i| (Arc::from(format!("w{i:05}").as_str()), (i as f32) / 10_000.0))
            .collect();
        let idx = SuggestIndex::new(terms);
        let t0 = std::time::Instant::now();
        let r = idx.query("w", 5);
        let elapsed_us = t0.elapsed().as_micros();
        assert_eq!(r.len(), 5);
        // Top-5 should be the highest-scored ones: w09999..w09995.
        assert_eq!(r[0].term, "w09999");
        // 10k scan + quickselect + 5 clones should finish well under 10ms.
        assert!(elapsed_us < 10_000, "query took {elapsed_us}µs, expected <10ms");
    }
}
