# Technical Design: Query Understanding

## 1. Architecture

QU sits between the raw `SearchRequest` and the WAND retrieval call. It lives entirely inside the `search-query` crate — the retrieval layer never sees a raw string, only a `ParsedQuery`.

```
SearchRequest { query: "Computadora i7 gamer", ... }
        │
        ▼
┌─────────────────────────────────┐
│        QU Pipeline              │
│                                 │
│  ┌─────────────┐                │
│  │  Tokenizer  │  raw → tokens  │
│  └──────┬──────┘                │
│         ▼                       │
│  ┌──────────────────┐           │
│  │ LanguageDetector │           │
│  └──────┬───────────┘           │
│         ▼                       │
│  ┌──────────────────┐           │
│  │  BooleanMode     │  AND/OR   │
│  │  Selector        │           │
│  └──────┬───────────┘           │
│         ▼                       │
│  [... future analyzers ...]     │
└─────────┬───────────────────────┘
          │
          ▼
    ParsedQuery
          │
          ▼
   WAND Retrieval
```

## 2. Core Types

```rust
// crates/query/src/understanding.rs

pub struct ParsedQuery {
    pub original: String,
    pub tokens: Vec<String>,
    pub language: Language,
    pub boolean_mode: BooleanMode,
    pub transformations: Vec<String>,  // debug log of what was applied
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BooleanMode {
    And,
    OrFallback,  // set by executor when AND returned zero candidates
}

pub trait QueryAnalyzer: Send + Sync {
    fn analyze(&self, query: ParsedQuery) -> ParsedQuery;
}
```

## 3. Default Pipeline

Built at `QueryExecutor` construction time:

```rust
pub struct QUPipeline {
    analyzers: Vec<Box<dyn QueryAnalyzer>>,
}

impl QUPipeline {
    pub fn default(schema: &IndexSchema) -> Self {
        Self {
            analyzers: vec![
                Box::new(TokenizerAnalyzer::new(schema)),
                Box::new(LanguageAnalyzer::default()),
            ],
        }
    }

    pub fn run(&self, query: &str) -> ParsedQuery {
        let mut pq = ParsedQuery::new(query);
        for analyzer in &self.analyzers {
            pq = analyzer.analyze(pq);
        }
        pq
    }
}
```

## 4. AND → OR Fallback

The fallback is handled in `QueryExecutor::score_query_wand`, not in the pipeline itself (the pipeline doesn't know about posting lists). Flow:

```
1. pipeline.run(query) → ParsedQuery { mode: And, ... }
2. wand_top_k(AND mode) → results
3. if results.is_empty() AND tokens.len() > 1:
       a. retry with OR: for each token independently, collect top-K
       b. merge by score, deduplicate
       c. set parsed_query.boolean_mode = OrFallback
4. return results + mode
```

OR mode collects per-token results independently (one WAND call per token) and merges with score sum. This is already close to the existing OR candidate collection, but now it's an explicit fallback step rather than the default behavior.

## 5. SearchResponse Change

Add `retrieval_mode` to the response so clients can display a "showing partial results" notice:

```rust
pub struct SearchResponse {
    // existing fields ...
    pub retrieval_mode: RetrievalMode,  // "and" | "or_fallback"
}

pub enum RetrievalMode {
    And,
    OrFallback,
}
```

Serializes as `"retrieval_mode": "and"` or `"retrieval_mode": "or_fallback"`.

## 6. Files Changed

| File | Change |
|------|--------|
| `crates/query/src/understanding.rs` | New file: `ParsedQuery`, `BooleanMode`, `QueryAnalyzer`, `QUPipeline` |
| `crates/query/src/lib.rs` | Export `understanding` module |
| `crates/query/src/executor.rs` | Use `QUPipeline`; implement OR fallback after zero AND results |
| `crates/core/src/lib.rs` | Add `RetrievalMode` to `SearchResponse` |
| `crates/proto/proto/shard.proto` | Add `retrieval_mode` string field to `SearchResponse` |
| `crates/shard/src/grpc_server.rs` | Map new field in proto conversion |
| `crates/router/src/router.rs` | Pass through `retrieval_mode` in merged response |
