use search_core::SortSpec;

/// A scored document candidate from a segment.
#[derive(Debug, Clone)]
pub struct ScoredDoc {
    pub segment_ord: usize,
    pub local_doc_id: u32,
    pub global_doc_id: u64,
    pub score: f32,
    /// Populated when a sort-by-field spec is present.
    pub sort_value: Option<f64>,
}

/// Collects scored documents and applies sorting + pagination.
pub struct TopNCollector;

impl TopNCollector {
    /// From a flat list of scored docs, return (page, total_hits).
    pub fn collect(
        mut docs: Vec<ScoredDoc>,
        sort: Option<&SortSpec>,
        offset: usize,
        limit: usize,
    ) -> (Vec<ScoredDoc>, u64) {
        let total = docs.len() as u64;

        if let Some(spec) = sort {
            use search_core::SortOrder;
            docs.sort_unstable_by(|a, b| {
                let av = a.sort_value.unwrap_or(f64::NEG_INFINITY);
                let bv = b.sort_value.unwrap_or(f64::NEG_INFINITY);
                match spec.order {
                    SortOrder::Asc => av.partial_cmp(&bv).unwrap_or(std::cmp::Ordering::Equal),
                    SortOrder::Desc => bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal),
                }
            });
        } else {
            // Default: sort by score descending
            docs.sort_unstable_by(|a, b| {
                b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let page = docs.into_iter().skip(offset).take(limit).collect();
        (page, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{SortOrder, SortSpec};

    fn make_docs(scores: &[f32]) -> Vec<ScoredDoc> {
        scores
            .iter()
            .enumerate()
            .map(|(i, &score)| ScoredDoc {
                segment_ord: 0,
                local_doc_id: i as u32,
                global_doc_id: i as u64,
                score,
                sort_value: Some(score as f64),
            })
            .collect()
    }

    #[test]
    fn test_top_n_by_score() {
        let docs = make_docs(&[0.1, 0.9, 0.5, 0.3, 0.8]);
        let (page, total) = TopNCollector::collect(docs, None, 0, 3);
        assert_eq!(total, 5);
        assert_eq!(page.len(), 3);
        assert!(page[0].score >= page[1].score && page[1].score >= page[2].score);
        assert_eq!(page[0].score, 0.9);
    }

    #[test]
    fn test_pagination() {
        let docs = make_docs(&[5.0, 4.0, 3.0, 2.0, 1.0]);
        let (page, total) = TopNCollector::collect(docs, None, 2, 2);
        assert_eq!(total, 5);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].score, 3.0);
        assert_eq!(page[1].score, 2.0);
    }

    #[test]
    fn test_sort_asc() {
        let docs = make_docs(&[3.0, 1.0, 2.0]);
        let sort = SortSpec { field: "price".into(), order: SortOrder::Asc };
        let (page, _) = TopNCollector::collect(docs, Some(&sort), 0, 10);
        assert_eq!(page[0].sort_value, Some(1.0));
        assert_eq!(page[2].sort_value, Some(3.0));
    }

    #[test]
    fn test_sort_desc() {
        let docs = make_docs(&[3.0, 1.0, 2.0]);
        let sort = SortSpec { field: "price".into(), order: SortOrder::Desc };
        let (page, _) = TopNCollector::collect(docs, Some(&sort), 0, 10);
        assert_eq!(page[0].sort_value, Some(3.0));
    }

    #[test]
    fn test_offset_beyond_results() {
        let docs = make_docs(&[1.0, 2.0]);
        let (page, total) = TopNCollector::collect(docs, None, 10, 5);
        assert_eq!(total, 2);
        assert!(page.is_empty());
    }
}
