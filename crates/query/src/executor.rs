use crate::collector::ScoredDoc;
use crate::scorer::{Bm25Scorer, SegmentStatistics};
use crate::understanding::QUPipeline;
use crate::wand::{TermCursor, wand_top_k};
use roaring::RoaringBitmap;
use search_analysis::AnalyzerFactory;
use search_core::{AnalyzerType, FieldType, FilterValue, IndexSchema, RetrievalMode, SearchRequest, SortSpec};
use search_index::SegmentReader;
use search_index::posting::PostingList;
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
    pub retrieval_mode: RetrievalMode,
}

pub struct QueryExecutor {
    schema: IndexSchema,
    scorer: Bm25Scorer,
    qu_pipeline: QUPipeline,
}

impl QueryExecutor {
    pub fn new(schema: IndexSchema) -> Self {
        let qu_pipeline = QUPipeline::default_for_schema(&schema);
        Self { schema, scorer: Bm25Scorer::default(), qu_pipeline }
    }

    pub fn execute(
        &self,
        reader: &SegmentReader,
        stats: &SegmentStatistics,
        segment_ord: usize,
        request: &SearchRequest,
    ) -> search_core::Result<SegmentSearchResult> {
        let filter_bitmap = self.build_filter_bitmap(reader, request);

        let (scored, retrieval_mode) = if request.query.is_empty() {
            let live = reader.all_live_docs();
            let matching = if let Some(ref f) = filter_bitmap { live & f } else { live };
            let docs = matching
                .iter()
                .map(|id| ScoredDoc {
                    segment_ord,
                    local_doc_id: id,
                    global_doc_id: reader.get_doc(id).map(|d| d.id).unwrap_or(id as u64),
                    score: 1.0,
                    sort_value: self.sort_value(reader, id, request.sort.as_ref()),
                })
                .collect();
            (docs, RetrievalMode::And)
        } else {
            let parsed = self.qu_pipeline.run(&request.query);
            let and_results = self.score_query_wand(
                reader, stats, segment_ord, request, filter_bitmap.as_ref(), &parsed.tokens,
            )?;

            // OR fallback: if AND returned nothing and query has multiple tokens, retry with OR
            if and_results.is_empty() && parsed.tokens.len() > 1 {
                let or_results = self.score_query_or_fallback(
                    reader, stats, segment_ord, request, filter_bitmap.as_ref(), &parsed.tokens,
                )?;
                (or_results, RetrievalMode::OrFallback)
            } else {
                (and_results, RetrievalMode::And)
            }
        };

        let total_hits = scored.len() as u64;
        let matching_bitmap: RoaringBitmap = scored.iter().map(|d| d.local_doc_id).collect();
        let aggregations = request
            .aggregations
            .iter()
            .map(|field| {
                let counts = reader.aggregate_counts(field, &matching_bitmap);
                (field.clone(), counts)
            })
            .collect();

        Ok(SegmentSearchResult { scored_docs: scored, total_hits, aggregations, retrieval_mode })
    }

    fn score_query_wand(
        &self,
        reader: &SegmentReader,
        stats: &SegmentStatistics,
        segment_ord: usize,
        request: &SearchRequest,
        filter: Option<&RoaringBitmap>,
        token_texts: &[String],
    ) -> search_core::Result<Vec<ScoredDoc>> {
        if token_texts.is_empty() {
            return Ok(vec![]);
        }

        // Re-analyze to get token structs for scoring (needed for starts_with matching)
        let query_analyzer = self
            .schema
            .fields
            .iter()
            .find(|f| f.indexed && f.field_type == FieldType::Text)
            .map(|f| AnalyzerFactory::build(&f.analyzer))
            .unwrap_or_else(|| AnalyzerFactory::build(&AnalyzerType::Keyword));
        let query_tokens = query_analyzer.analyze(&request.query);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        // ── Build one merged posting list per query token ──────────────────
        // Prefix search so "computadora" matches "computadoras", etc.
        let mut cursors: Vec<TermCursor> = Vec::with_capacity(query_tokens.len());

        for token in &query_tokens {
            let raw_lists: Vec<PostingList> = {
                let prefix = reader.prefix_search_term(&token.text);
                if !prefix.is_empty() {
                    prefix.into_iter().map(|(_, pl)| pl).collect()
                } else if request.typo_tolerance && token.text.len() > 4 {
                    let max_dist = if token.text.len() <= 6 { 1 } else { 2 };
                    reader.fuzzy_search_term(&token.text, max_dist)
                        .into_iter().map(|(_, pl)| pl).collect()
                } else {
                    reader.search_term(&token.text)
                        .map(|pl| vec![pl])
                        .unwrap_or_default()
                }
            };

            if raw_lists.is_empty() {
                // AND semantics: if any token has no matches, result is empty.
                return Ok(vec![]);
            }

            // Merge posting lists for this token (prefix expansion → multiple terms)
            let total_df: u32 = raw_lists.iter().map(|pl| pl.len() as u32).sum();
            let mut merged = PostingList::merge(raw_lists);
            merged.rebuild_blocks(stats.field_avg_lengths.get("title").copied().unwrap_or(8.0));

            let idf = self.scorer.idf(stats.total_docs, total_df);
            let avgdl = stats.field_avg_lengths.get("title").copied().unwrap_or(8.0);
            cursors.push(TermCursor::new(merged, idf, avgdl, self.scorer.clone()));
        }

        let title_boost = self.schema.fields.iter()
            .find(|f| f.name == "title").map(|f| f.boost).unwrap_or(2.0);
        let desc_boost = self.schema.fields.iter()
            .find(|f| f.name == "description").map(|f| f.boost).unwrap_or(1.0);

        // ── WAND top-K ─────────────────────────────────────────────────────
        // Retrieve enough candidates for accurate pagination total_hits.
        // At 1B docs WAND still skips >99% of postings even with k=200.
        let k = (request.offset + request.limit).max(request.limit * 10).max(200);

        let wand_results = wand_top_k(
            &mut cursors,
            k,
            title_boost,
            desc_boost,
            |local_doc_id| {
                // Filter check
                if filter.map(|f| !f.contains(local_doc_id)).unwrap_or(false) {
                    return None;
                }
                let doc = reader.get_doc(local_doc_id)?;

                // ── Exact per-field BM25 scoring ──
                let mut total_score = 0.0f32;
                for field in &self.schema.fields {
                    if !field.indexed || field.field_type != FieldType::Text {
                        continue;
                    }
                    let field_text = match field.name.as_str() {
                        "title" => &doc.title,
                        "description" => &doc.description,
                        _ => continue,
                    };
                    let analyzer = AnalyzerFactory::build(&field.analyzer);
                    let doc_tokens = analyzer.analyze(field_text);
                    let dl = doc_tokens.len() as u32;
                    let avgdl = stats.field_avg_lengths.get(&field.name).copied().unwrap_or(1.0);

                    for token in &query_tokens {
                        let tf = doc_tokens
                            .iter()
                            .filter(|t| t.text == token.text || t.text.starts_with(token.text.as_str()))
                            .count() as u32;
                        if tf == 0 {
                            if request.typo_tolerance && token.text.len() >= 3 {
                                let max_dist = if token.text.len() <= 4 { 1 } else { 2 };
                                let fuzzy_tf = doc_tokens
                                    .iter()
                                    .filter(|t| edit_distance(&t.text, &token.text) <= max_dist)
                                    .count() as u32;
                                if fuzzy_tf > 0 {
                                    let df = reader.search_term(&token.text)
                                        .map(|pl| pl.len() as u32).unwrap_or(1);
                                    let idf = self.scorer.idf(stats.total_docs, df);
                                    total_score += field.boost
                                        * self.scorer.term_score(fuzzy_tf, dl, avgdl, idf);
                                }
                            }
                            continue;
                        }
                        let df = reader.search_term(&token.text)
                            .map(|pl| pl.len() as u32).unwrap_or(1);
                        let idf = self.scorer.idf(stats.total_docs, df);
                        total_score += field.boost * self.scorer.term_score(tf, dl, avgdl, idf);
                    }
                }

                if total_score <= 0.0 {
                    return None;
                }

                // ── Title-position boost ──
                // Products whose name STARTS with the query (e.g. "Computadoras Monitor…")
                // rank above products that merely MENTION the query term (e.g. "Mesa … Computadora …").
                let title_analyzer = AnalyzerFactory::build(
                    &self.schema.fields.iter()
                        .find(|f| f.name == "title")
                        .map(|f| f.analyzer.clone())
                        .unwrap_or(AnalyzerType::Keyword),
                );
                let title_tokens = title_analyzer.analyze(&doc.title);
                let first: Vec<&str> = title_tokens.iter().take(3).map(|t| t.text.as_str()).collect();
                if !query_tokens.is_empty()
                    && query_tokens.iter().all(|qt| {
                        first.iter().any(|tt| *tt == qt.text || tt.starts_with(qt.text.as_str()))
                    })
                {
                    total_score *= 1.5;
                }

                Some(total_score)
            },
        );

        let result: Vec<ScoredDoc> = wand_results
            .into_iter()
            .filter_map(|(local_doc_id, score)| {
                let doc = reader.get_doc(local_doc_id)?;
                Some(ScoredDoc {
                    segment_ord,
                    local_doc_id,
                    global_doc_id: doc.id,
                    score,
                    sort_value: self.sort_value(reader, local_doc_id, request.sort.as_ref()),
                })
            })
            .collect();

        Ok(result)
    }

    /// OR fallback: run WAND independently for each token, merge by score sum.
    fn score_query_or_fallback(
        &self,
        reader: &SegmentReader,
        stats: &SegmentStatistics,
        segment_ord: usize,
        request: &SearchRequest,
        filter: Option<&RoaringBitmap>,
        token_texts: &[String],
    ) -> search_core::Result<Vec<ScoredDoc>> {
        let k = (request.offset + request.limit).max(request.limit * 10).max(200);
        let title_boost = self.schema.fields.iter()
            .find(|f| f.name == "title").map(|f| f.boost).unwrap_or(2.0);
        let desc_boost = self.schema.fields.iter()
            .find(|f| f.name == "description").map(|f| f.boost).unwrap_or(1.0);

        let query_analyzer = self
            .schema
            .fields
            .iter()
            .find(|f| f.indexed && f.field_type == FieldType::Text)
            .map(|f| AnalyzerFactory::build(&f.analyzer))
            .unwrap_or_else(|| AnalyzerFactory::build(&AnalyzerType::Keyword));
        let query_tokens = query_analyzer.analyze(&request.query);

        let mut score_map: HashMap<u32, f32> = HashMap::new();

        for token_text in token_texts {
            let raw_lists: Vec<PostingList> = {
                let prefix = reader.prefix_search_term(token_text);
                if !prefix.is_empty() {
                    prefix.into_iter().map(|(_, pl)| pl).collect()
                } else if request.typo_tolerance && token_text.len() > 4 {
                    let max_dist = if token_text.len() <= 6 { 1 } else { 2 };
                    reader.fuzzy_search_term(token_text, max_dist)
                        .into_iter().map(|(_, pl)| pl).collect()
                } else {
                    reader.search_term(token_text).map(|pl| vec![pl]).unwrap_or_default()
                }
            };

            if raw_lists.is_empty() {
                continue;
            }

            let total_df: u32 = raw_lists.iter().map(|pl| pl.len() as u32).sum();
            let mut merged = PostingList::merge(raw_lists);
            merged.rebuild_blocks(stats.field_avg_lengths.get("title").copied().unwrap_or(8.0));
            let idf = self.scorer.idf(stats.total_docs, total_df);
            let avgdl = stats.field_avg_lengths.get("title").copied().unwrap_or(8.0);
            let cursor = TermCursor::new(merged, idf, avgdl, self.scorer.clone());

            let token_results = wand_top_k(
                &mut vec![cursor],
                k,
                title_boost,
                desc_boost,
                |local_doc_id| {
                    if filter.map(|f| !f.contains(local_doc_id)).unwrap_or(false) {
                        return None;
                    }
                    let doc = reader.get_doc(local_doc_id)?;
                    let mut total_score = 0.0f32;
                    for field in &self.schema.fields {
                        if !field.indexed || field.field_type != FieldType::Text { continue; }
                        let field_text = match field.name.as_str() {
                            "title" => &doc.title,
                            "description" => &doc.description,
                            _ => continue,
                        };
                        let analyzer = AnalyzerFactory::build(&field.analyzer);
                        let doc_tokens = analyzer.analyze(field_text);
                        let dl = doc_tokens.len() as u32;
                        let avgdl = stats.field_avg_lengths.get(&field.name).copied().unwrap_or(1.0);
                        for qt in &query_tokens {
                            let tf = doc_tokens.iter()
                                .filter(|t| t.text == qt.text || t.text.starts_with(qt.text.as_str()))
                                .count() as u32;
                            if tf == 0 { continue; }
                            let df = reader.search_term(&qt.text).map(|pl| pl.len() as u32).unwrap_or(1);
                            let idf = self.scorer.idf(stats.total_docs, df);
                            total_score += field.boost * self.scorer.term_score(tf, dl, avgdl, idf);
                        }
                    }
                    if total_score > 0.0 { Some(total_score) } else { None }
                },
            );

            for (local_doc_id, score) in token_results {
                *score_map.entry(local_doc_id).or_default() += score;
            }
        }

        let result: Vec<ScoredDoc> = score_map
            .into_iter()
            .filter_map(|(local_doc_id, score)| {
                let doc = reader.get_doc(local_doc_id)?;
                Some(ScoredDoc {
                    segment_ord,
                    local_doc_id,
                    global_doc_id: doc.id,
                    score,
                    sort_value: self.sort_value(reader, local_doc_id, request.sort.as_ref()),
                })
            })
            .collect();

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
    use search_index::SegmentWriter;
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
        SearchRequest { query: query.into(), limit: 10, ..Default::default() }
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

        let result = executor.execute(&reader, &stats, 0, &default_request("samsung")).unwrap();
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

        let result = executor.execute(&reader, &stats, 0, &default_request("samsung")).unwrap();
        assert_eq!(result.scored_docs.len(), 2);
        let title_score = result.scored_docs.iter().find(|d| d.global_doc_id == 1).unwrap().score;
        let desc_score  = result.scored_docs.iter().find(|d| d.global_doc_id == 2).unwrap().score;
        assert!(title_score > desc_score, "title boost should win: {title_score} vs {desc_score}");
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
        req.filters.insert("category".into(), search_core::FilterValue::Equality { eq: "electronics".into() });
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
        let docs = vec![make_doc(1, "samsung phone", "Android", "electronics", 500.0)];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let mut req = default_request("samsang");
        req.typo_tolerance = true;
        let result = executor.execute(&reader, &stats, 0, &req).unwrap();
        assert_eq!(result.scored_docs.len(), 1);
    }

    #[test]
    fn test_and_semantics_multi_term() {
        let docs = vec![
            make_doc(1, "Apple iPhone 15", "", "electronics", 1000.0),
            make_doc(2, "Bafle Iron 15 Pulgadas", "", "audio", 200.0),
            make_doc(3, "iPhone case accessories", "", "accessories", 20.0),
        ];
        let (_dir, reader) = write_and_open(&docs);
        let schema = make_schema();
        let stats = SegmentStatistics::compute(&reader, &schema);
        let executor = QueryExecutor::new(schema);

        let result = executor.execute(&reader, &stats, 0, &default_request("iphone 15")).unwrap();
        // Only doc 1 has both "iphone" AND "15"
        assert_eq!(result.scored_docs.len(), 1);
        assert_eq!(result.scored_docs[0].global_doc_id, 1);
    }
}
