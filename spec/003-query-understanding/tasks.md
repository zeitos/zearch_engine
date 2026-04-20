# Tasks: Query Understanding

## Status: Deployed

- [x] T-01 — `understanding.rs`: `ParsedQuery`, `BooleanMode`, `QueryAnalyzer` trait, `QUPipeline`, `TokenizerAnalyzer`, `LanguageAnalyzer`
- [x] T-02 — Export `ParsedQuery`, `QUPipeline`, `BooleanMode` from `search-query`
- [x] T-03 — `RetrievalMode` enum (`And` / `OrFallback`) added to `SearchResponse`
- [x] T-04 — QU pipeline wired into query executor; replaces raw string tokenization
- [x] T-05 — OR fallback: zero AND results + multiple tokens → per-token WAND, merge by score sum, set `retrieval_mode = OrFallback`
- [x] T-06 — Proto: `retrieval_mode` string field in `SearchResponse`; gRPC handler maps enum → string
- [x] T-07 — Router propagates `retrieval_mode`; any shard `or_fallback` → merged response is `or_fallback`
- [x] T-08 — Tests: QU pipeline tokenizes correctly; OR fallback triggers on zero AND results; does not trigger when AND has results; `retrieval_mode` correct in response
