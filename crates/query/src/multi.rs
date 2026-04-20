use crate::collector::TopNCollector;
use crate::executor::{QueryExecutor, SegmentSearchResult};
use crate::scorer::SegmentStatistics;
use search_core::{
    AggregationBucket, Document, IndexSchema, RetrievalMode, SearchRequest, SearchResponse, SearchHit,
};
use search_index::SegmentReader;
use std::collections::HashMap;

pub struct MultiSegmentSearcher {
    executor: QueryExecutor,
}

impl MultiSegmentSearcher {
    pub fn new(schema: IndexSchema) -> Self {
        Self { executor: QueryExecutor::new(schema) }
    }

    /// Search across multiple segments (each paired with its stats).
    /// `segment_ord` in ScoredDoc matches the index into `segments`.
    pub fn search(
        &self,
        segments: &[(&SegmentReader, &SegmentStatistics)],
        request: &SearchRequest,
    ) -> search_core::Result<SearchResponse> {
        let start = std::time::Instant::now();

        let mut all_scored = Vec::new();
        let mut total_hits: u64 = 0;
        let mut merged_aggs: HashMap<String, HashMap<String, u64>> = HashMap::new();
        let mut any_or_fallback = false;

        for (ord, (reader, stats)) in segments.iter().enumerate() {
            let SegmentSearchResult { scored_docs, total_hits: seg_hits, aggregations, retrieval_mode } =
                self.executor.execute(reader, stats, ord, request)?;

            total_hits += seg_hits;
            all_scored.extend(scored_docs);
            if retrieval_mode == RetrievalMode::OrFallback {
                any_or_fallback = true;
            }

            for (field, counts) in aggregations {
                let bucket = merged_aggs.entry(field).or_default();
                for (value, count) in counts {
                    *bucket.entry(value).or_default() += count;
                }
            }
        }

        let (page, _) = TopNCollector::collect(
            all_scored,
            request.sort.as_ref(),
            request.offset,
            request.limit,
        );

        // Fetch full documents for the result page
        let hits: Vec<SearchHit> = page
            .into_iter()
            .filter_map(|scored| {
                let (reader, _) = segments.get(scored.segment_ord)?;
                let doc: Document = reader.get_doc(scored.local_doc_id)?;
                Some(SearchHit { id: doc.id, score: scored.score, document: Some(doc) })
            })
            .collect();

        let aggregations = merged_aggs
            .into_iter()
            .map(|(field, counts)| {
                let mut buckets: Vec<AggregationBucket> = counts
                    .into_iter()
                    .map(|(value, count)| AggregationBucket { value, count })
                    .collect();
                buckets.sort_by(|a, b| b.count.cmp(&a.count));
                (field, buckets)
            })
            .collect();

        Ok(SearchResponse {
            hits,
            total_hits,
            aggregations,
            reranked: false,
            took_ms: start.elapsed().as_millis() as u64,
            retrieval_mode: if any_or_fallback { RetrievalMode::OrFallback } else { RetrievalMode::And },
            cache_hit: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use search_core::{Document, IndexSchema, SearchRequest};
    use search_index::{SegmentReader, SegmentWriter};
    use crate::scorer::SegmentStatistics;
    use std::collections::HashMap;

    fn make_doc(id: u64, title: &str, category: &str) -> Document {
        Document {
            id,
            title: title.into(),
            description: "desc".into(),
            price: id as f64 * 10.0,
            category: category.into(),
            attributes: HashMap::new(),
        }
    }

    fn write_segment(docs: &[Document]) -> (tempfile::TempDir, SegmentReader, SegmentStatistics) {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");
        let schema = IndexSchema::default_product_schema();
        SegmentWriter::new(schema.clone()).write("seg", docs, &seg_dir).unwrap();
        let reader = SegmentReader::open(&seg_dir).unwrap();
        let stats = SegmentStatistics::compute(&reader, &schema);
        (dir, reader, stats)
    }

    #[test]
    fn test_two_segment_search() {
        let docs1 = vec![make_doc(1, "Samsung Galaxy", "electronics")];
        let docs2 = vec![make_doc(2, "Samsung Note", "electronics")];

        let (_d1, r1, s1) = write_segment(&docs1);
        let (_d2, r2, s2) = write_segment(&docs2);

        let schema = IndexSchema::default_product_schema();
        let searcher = MultiSegmentSearcher::new(schema);
        let req = SearchRequest { query: "samsung".into(), limit: 10, ..Default::default() };

        let response = searcher.search(&[(&r1, &s1), (&r2, &s2)], &req).unwrap();
        assert_eq!(response.total_hits, 2);
        assert_eq!(response.hits.len(), 2);
    }

    #[test]
    fn test_aggregation_merge() {
        let docs1 = vec![
            make_doc(1, "Samsung phone", "electronics"),
            make_doc(2, "Nike shoes", "clothing"),
        ];
        let docs2 = vec![
            make_doc(3, "Apple phone", "electronics"),
        ];

        let (_d1, r1, s1) = write_segment(&docs1);
        let (_d2, r2, s2) = write_segment(&docs2);

        let schema = IndexSchema::default_product_schema();
        let searcher = MultiSegmentSearcher::new(schema);
        let req = SearchRequest {
            query: "".into(),
            aggregations: vec!["category".into()],
            limit: 10,
            ..Default::default()
        };

        let response = searcher.search(&[(&r1, &s1), (&r2, &s2)], &req).unwrap();
        let cats = response.aggregations.get("category").unwrap();
        let elec = cats.iter().find(|b| b.value == "electronics").unwrap();
        let cloth = cats.iter().find(|b| b.value == "clothing").unwrap();
        assert_eq!(elec.count, 2);
        assert_eq!(cloth.count, 1);
    }

    #[test]
    fn test_pagination_across_segments() {
        let docs1: Vec<_> = (1..=5).map(|i| make_doc(i, "samsung phone", "electronics")).collect();
        let docs2: Vec<_> = (6..=10).map(|i| make_doc(i, "samsung phone", "electronics")).collect();

        let (_d1, r1, s1) = write_segment(&docs1);
        let (_d2, r2, s2) = write_segment(&docs2);

        let schema = IndexSchema::default_product_schema();
        let searcher = MultiSegmentSearcher::new(schema);
        let req = SearchRequest {
            query: "samsung".into(),
            offset: 0,
            limit: 3,
            ..Default::default()
        };

        let response = searcher.search(&[(&r1, &s1), (&r2, &s2)], &req).unwrap();
        assert_eq!(response.total_hits, 10);
        assert_eq!(response.hits.len(), 3);
    }
}
