# Requirements: Query Understanding

## Feature Overview

Before hitting the retrieval layer, every query passes through a **Query Understanding** (QU) pipeline. The pipeline transforms a raw string into a structured `ParsedQuery` that retrieval can act on. Today this means boolean mode selection and AND→OR fallback. Tomorrow it means intent detection, semantic expansion, and pluggable ML analyzers — all wired in without touching the retrieval layer.

## User Stories

### US-1: AND with OR fallback
**As a** user searching for "Computadora i7 gamer"  
**I want to** get results even when no product matches all three terms simultaneously  
**So that** I never see a zero-result page for a reasonable query

**Acceptance Criteria:**
- Engine first attempts AND (all tokens must appear)
- If AND returns zero results, automatically retries with OR (any token must appear)
- Response includes a flag indicating which mode was used (`retrieval_mode: "and" | "or_fallback"`)
- Fallback adds no more than one extra round-trip of latency

### US-2: Structured parsed query
**As a** developer building on top of the engine  
**I want to** inspect the structured output of query understanding  
**So that** I can debug relevance issues and build QU-aware features

**Acceptance Criteria:**
- `ParsedQuery` includes: original query, tokens, detected language, retrieval mode, and any transformations applied
- Transformations are logged at DEBUG level with before/after

### US-3: Pluggable analyzer pipeline
**As a** team that wants to add ML-based query understanding  
**I want to** plug in a semantic analyzer (e.g. query expansion, intent classification)  
**So that** I can improve relevance without modifying the retrieval layer

**Acceptance Criteria:**
- Pipeline is a list of `QueryAnalyzer` trait objects applied in order
- Each analyzer receives the current `ParsedQuery` and returns a (possibly modified) `ParsedQuery`
- Analyzers are optional — omitting all of them falls back to the default behavior
- A no-op passthrough analyzer is provided as a reference implementation

### US-4: Language detection
**As a** user who may search in Spanish, English, or Portuguese  
**I want to** get results analyzed with the correct language stemmer  
**So that** plurals and inflections are handled correctly regardless of the query language

**Acceptance Criteria:**
- QU pipeline detects query language (default: Spanish if undetected)
- Detected language is passed to the retrieval layer to select the correct analyzer
- Detection can be overridden per-request via the `language` field in `SearchRequest`

## Functional Requirements

| ID | Requirement |
|----|------------|
| FR-01 | QU pipeline runs before every retrieval call (shard-side) |
| FR-02 | Default pipeline: tokenize → detect language → select boolean mode |
| FR-03 | AND mode: all tokens must appear in retrieved documents |
| FR-04 | OR fallback: triggered automatically when AND returns zero candidates |
| FR-05 | `ParsedQuery` struct carries tokens, language, boolean mode, and applied transformations |
| FR-06 | `QueryAnalyzer` trait: `fn analyze(&self, query: ParsedQuery) -> ParsedQuery` |
| FR-07 | Pipeline is configurable: list of analyzers injected at startup |
| FR-08 | `retrieval_mode` field added to `SearchResponse` to expose AND vs OR |
| FR-09 | No regression on existing AND behavior when all tokens match |

## Non-Functional Requirements

| ID | Requirement | Target |
|----|------------|--------|
| NFR-01 | QU overhead (no ML) | < 1ms per query |
| NFR-02 | OR fallback overhead | ≤ 1 extra shard round-trip (no network if local) |
| NFR-03 | Extensibility | Adding a new analyzer must not require changes outside `search-query` crate |

## Out of Scope (v1)

- Semantic/embedding-based query expansion
- Intent classification (navigational vs informational vs transactional)
- Query rewriting (e.g. "celular" → "smartphone OR celular")
- Spell correction (separate feature, tracked in 001)
- Personalization or session context

## Future Extensibility (P2)

These must be implementable by adding new `QueryAnalyzer` implementations without modifying the pipeline itself:

| Analyzer | Description |
|----------|-------------|
| `SemanticExpander` | Uses an embedding model to expand query with related terms |
| `IntentClassifier` | Detects navigational / transactional / informational intent |
| `SynonymExpander` | Expands tokens using a configurable synonym map |
| `QueryRewriter` | Rewrites queries based on historical click data |
