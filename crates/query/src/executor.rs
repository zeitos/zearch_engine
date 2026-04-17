use crate::collector::ScoredDoc;
use crate::scorer::{Bm25Scorer, SegmentStatistics};
use roaring::RoaringBitmap;
use search_analysis::AnalyzerFactory;
use search_core::{FieldType, FilterValue, IndexSchema, SearchRequest, SortSpec};
use search_index::SegmentReader;
use std::collections::HashMap;

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m { dp[i][0] = i; }
    for j in 0..=n { dp[0][j] = j; }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i-1] == b[j-1] {
                dp[i-1][j-1]
            } else {
                1 + dp[i-1][j].min(dp[i][j-1]).min(dp[i-1][j-1])
            };
        }
    }
    dp[m][n]
}

pub struct SegmentSearchResult {
    pub scored_docs: Vec<ScoredDoc>,
    pub total_hits: u64,
    pub aggregations: HashMap<String, Vec<(String, u64)>>,
}

pub struct QueryExecutor {
    schema: IndexSchema,
    scorer: Bm25Scorer,
}

impl QueryExecutor {
    pub fn new(schema: IndexSchema) -> Self {
        Self { schema, scorer: Bm25Scorer::default() }
    }

    pub fn execute(
        &self,
        reader: &SegmentReader,
        stats: &SegmentStatistics,
        segment_ord: usize,
        request: &SearchRequest,
    ) -> search_core::Result<SegmentSearchResult> {
        // Step 1: resolve filter bitmap
        let filter_bitmap = self.build_filter_bitmap(reader, request);

        // Step 2: score documents via full-text search
        let scored = if request.query.is_empty() {
            // No query: score all live (filtered) docs equally at 1.0
            let live = reader.all_live_docs();
            let matching = if let Some(ref f) = filter_bitmap {
                live & f
            } else {
                live
            };
            matching
                .iter()
                .map(|id| ScoredDoc {
                    segment_ord,
                    local_doc_id: id,
                    global_doc_id: reader.get_doc(id).map(|d| d.id).unwrap_or(id as u64),
                    score: 1.0,
                    sort_value: self.sort_value(reader, id, request.sort.as_ref()),
                })
                .collect()
        } else {
            self.score_query(reader, stats, segment_ord, request, filter_bitmap.as_ref())?
        };

        let total_hits = scored.len() as u64;

        // Step 3: aggregations on matching docs
        let matching_bitmap: RoaringBitmap = scored.iter().map(|d| d.local_doc_id).collect();
        let aggregations = request
            .aggregations
            .iter()
            .map(|field| {
                let counts = reader.aggregate_counts(field, &matching_bitmap);
                (field.clone(), counts)
            })
            .collect();

        Ok(SegmentSearchResult { scored_docs: scored, total_hits, aggregations })
    }

    fn score_query(
        &self,
        reader: &SegmentReader,
        stats: &SegmentStatistics,
        segment_ord: usize,
        request: &SearchRequest,
        filter: Option<&RoaringBitmap>,
    ) -> search_core::Result<Vec<ScoredDoc>> {
        // Use the first (or highest-boost) text field's analyzer to tokenize the query
        let query_analyzer = self
            .schema
            .fields
            .iter()
            .find(|f| f.indexed && f.field_type == FieldType::Text)
            .map(|f| AnalyzerFactory::build(&f.analyzer))
            .unwrap_or_else(|| AnalyzerFactory::build(&search_core::AnalyzerType::Keyword));

        let query_tokens = query_analyzer.analyze(&request.query);

        // Collect per-token doc sets, then intersect (AND semantics).
        // This prevents a doc matching only "15" from ranking above one matching "iphone 15".
        let mut per_token_sets: Vec<std::collections::HashSet<u32>> = Vec::new();
        for token in &query_tokens {
            let mut token_docs: std::collections::HashSet<u32> = std::collections::HashSet::new();
            let posting_lists = if token.text.len() <= 4 {
                // Short terms: prefix search so "bici" matches "bicicleta"
                let mut results = reader.prefix_search_term(&token.text);
                if results.is_empty() {
                    results = reader
                        .search_term(&token.text)
                        .map(|pl| vec![(token.text.clone(), pl)])
                        .unwrap_or_default();
                }
                results
            } else if request.typo_tolerance {
                let max_dist = if token.text.len() <= 6 { 1 } else { 2 };
                reader.fuzzy_search_term(&token.text, max_dist)
            } else {
                reader
                    .search_term(&token.text)
                    .map(|pl| vec![(token.text.clone(), pl)])
                    .unwrap_or_default()
            };
            for (_, pl) in posting_lists {
                for posting in &pl.postings {
                    token_docs.insert(posting.doc_id);
                }
            }
            per_token_sets.push(token_docs);
        }

        // Intersect all per-token sets so every query token must be present
        let candidate_doc_ids: std::collections::HashSet<u32> = if per_token_sets.is_empty() {
            std::collections::HashSet::new()
        } else {
            let mut iter = per_token_sets.into_iter();
            let first = iter.next().unwrap();
            iter.fold(first, |acc, set| acc.intersection(&set).copied().collect())
        };

        // For each candidate doc, compute per-field BM25 by re-analyzing the document's fields
        let mut result = Vec::new();
        for local_doc_id in candidate_doc_ids {
            if filter.map(|f| !f.contains(local_doc_id)).unwrap_or(false) {
                continue;
            }
            let Some(doc) = reader.get_doc(local_doc_id) else { continue };

            let mut total_score = 0.0f32;

            for field in &self.schema.fields {
                if !field.indexed || field.field_type != FieldType::Text {
                    continue;
                }
                let analyzer = AnalyzerFactory::build(&field.analyzer);
                let field_text = match field.name.as_str() {
                    "title" => &doc.title,
                    "description" => &doc.description,
                    _ => continue,
                };
                let doc_tokens = analyzer.analyze(field_text);
                let dl = doc_tokens.len() as u32;
                let avgdl = stats.field_avg_lengths.get(&field.name).copied().unwrap_or(1.0);

                for token in &query_tokens {
                    // Count how many times this query token appears in this field
                    let tf = doc_tokens.iter().filter(|t| t.text == token.text).count() as u32;
                    if tf == 0 {
                        // Also check fuzzy matches if typo tolerance is on
                        if request.typo_tolerance && token.text.len() >= 3 {
                            let max_dist = if token.text.len() <= 4 { 1 } else { 2 };
                            let fuzzy_tf = doc_tokens
                                .iter()
                                .filter(|t| {
                                    edit_distance(&t.text, &token.text) <= max_dist as usize
                                })
                                .count() as u32;
                            if fuzzy_tf > 0 {
                                // Approximate df: use global posting list len
                                let df = reader
                                    .search_term(&token.text)
                                    .map(|pl| pl.len() as u32)
                                    .unwrap_or(1);
                                let idf = self.scorer.idf(stats.total_docs, df);
                                total_score += field.boost
                                    * self.scorer.term_score(fuzzy_tf, dl, avgdl, idf);
                            }
                        }
                        continue;
                    }
                    let df = reader
                        .search_term(&token.text)
                        .map(|pl| pl.len() as u32)
                        .unwrap_or(1);
                    let idf = self.scorer.idf(stats.total_docs, df);
                    total_score += field.boost * self.scorer.term_score(tf, dl, avgdl, idf);
                }
            }

            if total_score > 0.0 {
                result.push(ScoredDoc {
                    segment_ord,
                    local_doc_id,
                    global_doc_id: doc.id,
                    score: total_score,
                    sort_value: self.sort_value(reader, local_doc_id, request.sort.as_ref()),
                });
            }
        }

        Ok(result)
    }

    fn build_filter_bitmap(
        &self,
        reader: &SegmentReader,
        request: &SearchRequest,
    ) -> Option<RoaringBitmap> {
        if request.filters.is_empty() {
            return None;
        }
        let mut combined = reader.all_live_docs();
        for (field, filter) in &request.filters {
            let bits = match filter {
                FilterValue::Equality { eq } => reader.filter_eq(field, eq),
                FilterValue::Range { gte, lte } => reader.filter_range(field, *gte, *lte),
                FilterValue::MultiValue { r#in } => reader.filter_in(field, r#in),
            };
            combined &= bits;
        }
        Some(combined)
    }

    fn sort_value(
        &self,
        reader: &SegmentReader,
        local_doc_id: u32,
        sort: Option<&SortSpec>,
    ) -> Option<f64> {
        let spec = sort?;
        let doc = reader.get_doc(local_doc_id)?;
        match spec.field.as_str() {
            "price" => Some(doc.price),
            other => {
                if let Some(search_core::Value::Number(v)) = doc.attributes.get(other) {
                    Some(*v)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scorer::SegmentStatistics;
    use search_core::{Document, IndexSchema, SearchRequest};
    use search_index::{SegmentWriter};
    use std::collections::HashMap;

    fn make_schema() -> IndexSchema {
        IndexSchema::default_product_schema()
    }

    fn make_doc(id: u64, title: &str, description: &str, category: &str, price: f64) -> Document {
        Document {
            id,
            title: title.into(),
            description: description.into(),
            price,
            category: category.into(),
            attributes: HashMap::new(),
        }
    }

    fn write_and_open(docs: &[Document]) -> (tempfile::TempDir, search_index::SegmentReader) {
        let dir = tempfile::TempDir::new().unwrap();
        let seg_dir = dir.path().join("seg");
        let schema = make_schema();
        SegmentWriter::new(schema).write("seg", docs, &seg_dir).unwrap();
        let reader = search_index::SegmentReader::open(&seg_dir).unwrap();
        (dir, reader)
    }

    fn default_request(query: &str) -> SearchRequest {
        SearchRequest {
            query: query.into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_single_term_query() {
        let docs = vec![
            make_doc(1, "Samsung Galaxy phone", "Great Android phone", "electronics", 999.0),
            make_doc(2, "Apple iPhone", "iOS device", "electronics", 1199.0),
            make_doc(3, "Nike shoes", "Running shoes", "clothing", 89.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let result = executor
            .execute(&reader, &stats, 0, &default_request("samsung"))
            .unwrap();
        assert_eq!(result.scored_docs.len(), 1);
        assert_eq!(result.scored_docs[0].global_doc_id, 1);
    }

    #[test]
    fn test_title_boost_over_description() {
        let docs = vec![
            make_doc(1, "samsung phone", "generic device", "electronics", 100.0),
            make_doc(2, "generic phone", "samsung android", "electronics", 100.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let result = executor
            .execute(&reader, &stats, 0, &default_request("samsung"))
            .unwrap();
        assert_eq!(result.scored_docs.len(), 2);
        let title_match = result.scored_docs.iter().find(|d| d.global_doc_id == 1).unwrap();
        let desc_match = result.scored_docs.iter().find(|d| d.global_doc_id == 2).unwrap();
        assert!(title_match.score > desc_match.score, "title boost should win");
    }

    #[test]
    fn test_with_filter() {
        let docs = vec![
            make_doc(1, "Samsung phone", "Android", "electronics", 500.0),
            make_doc(2, "Samsung shirt", "Cotton", "clothing", 30.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let mut req = default_request("samsung");
        req.filters.insert(
            "category".into(),
            search_core::FilterValue::Equality { eq: "electronics".into() },
        );
        let result = executor.execute(&reader, &stats, 0, &req).unwrap();
        assert_eq!(result.scored_docs.len(), 1);
        assert_eq!(result.scored_docs[0].global_doc_id, 1);
    }

    #[test]
    fn test_with_aggregations() {
        let docs = vec![
            make_doc(1, "Samsung phone", "Android", "electronics", 500.0),
            make_doc(2, "Apple phone", "iOS", "electronics", 800.0),
            make_doc(3, "Nike shoes", "Running", "clothing", 90.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let mut req = default_request("phone");
        req.aggregations = vec!["category".into()];
        let result = executor.execute(&reader, &stats, 0, &req).unwrap();

        let cats = result.aggregations.get("category").unwrap();
        let elec = cats.iter().find(|(v, _)| v == "electronics").unwrap();
        assert_eq!(elec.1, 2);
    }

    #[test]
    fn test_empty_query_returns_all() {
        let docs = vec![
            make_doc(1, "Samsung", "desc", "electronics", 100.0),
            make_doc(2, "Apple", "desc", "electronics", 200.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let result = executor.execute(&reader, &stats, 0, &default_request("")).unwrap();
        assert_eq!(result.scored_docs.len(), 2);
    }

    #[test]
    fn test_fuzzy_query() {
        let docs = vec![
            make_doc(1, "samsung phone", "Android", "electronics", 500.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let mut req = default_request("samsang"); // typo, distance 1
        req.typo_tolerance = true;
        let result = executor.execute(&reader, &stats, 0, &req).unwrap();
        assert_eq!(result.scored_docs.len(), 1);
    }
}
