# Agent A Cycle36 — pinned Python active-path RAG mode audit

## Reproducible source boundary

- Python repository: `Atm4x/NeuroMita`, CI-exported **full Git bundle** at `97310d6aaf6bf7eeb15ae84cf606adc0f024e135` (repository HEAD on 2026-11-08 MSK). Local bundle checksum and Git objects were verified. This is a static source read, **not** a live Python model/database run or a proof of a user's saved settings.
- Rust source: `Atm4x/neuromita-rust`, exact PR #276 `f55e96cea5db2976308612e598b0841b2622e9f8`, parent `2db8edebb8b9cf9e49d8af0b5dbb43493efc367e`.
- Python sources below were inspected **read-only**; no changes to Python, Legacy, or Obsolete components.

## Active Python execution path (not merely default values)

1. `src/controllers/model_controller.py:1345-1360,1882-1955` admits automatic retrieval only for a non-GameMaster character with `RAG_ENABLED` and `policy.react_level != 1`. `process_rag` prefers `user_input`, otherwise `system_input`; its output is a distinct prepared RAG context. The chosen character ID, not the GUI-wide selection, determines the lookup owner.
2. `src/managers/rag/rag_manager.py:821-885` rejects **automatic** `search_relevant` when `config_overrides is None` and RAG is disabled, but permits explicit/tool search even with `{}` overrides. It constructs a `RAGConfig` from actual `SettingsManager` values and only then applies recognized per-call override keys.
3. `src/managers/rag/pipeline/query_builder.py:14-81` derives keyword candidates from both the user query and expanded active-history tail, with lemmatization, length and term count settings. Query embedding may be absent. Last active message contributes actor metadata. Rust's current `LlmConversation::memory_query` is not a verified equivalent of this query builder.
4. `rag_manager.py:899-966` enables the *vector* retriever only with a valid query embedding **and** `vector_search_enabled`; *keyword_only* only with enabled keywords; FTS when enabled; graph only with an enabled graph retriever. The vector-only mode removes non-vector retrievers before execution. Each per-retriever exception is caught and replaced with an empty bucket.
5. `pipeline/retrievers/keyword_only.py:15-114` delegates to repository `keyword_rows`, which targets legacy records **without an embedding**. This is not all-corpus keyword retrieval. `pipeline/retrievers/fts.py` independently searches SQLite FTS; `pipeline/retrievers/vector.py` handles current-model vector candidates and optional sentence-level selection.
6. `rag_manager.py:948-1081` selects `RRF`, `vector_only`, `intersect`, `two_stage` or `union`; enriched candidates receive time/entity/priority features, a linear score, and an optional cross-encoder pass. Cross-encoder errors leave the already-ranked candidates available; this is **Python's** observed policy, not an authorization for Rust to silently substitute results. Graph minimum-result reservation and asynchronous access-stat updates follow ranking.
7. `pipeline/config.py:39-81,309-401,410-475` distinguishes named **preset application** from the `RAG_DEFAULTS` dictionary used by the UI's reset action. The reset dictionary has `RAG_ENABLED=True`, `RAG_PIPELINE_PRESET='Keyword+FTS only'`, `RAG_VECTOR_SEARCH_ENABLED=False`, `RAG_COMBINE_MODE='two_stage'`, `RAG_MEMORY_MODE='all'`, `RAG_USE_FTS=True`, `RAG_KEYWORD_SEARCH=True`, `RAG_USE_RRF=False`, `RAG_MAX_RESULTS=5`. Applying the actual *Keyword+FTS only* preset writes `RAG_COMBINE_MODE='union'` and disables CE. Consequently the reset dictionary's selected-preset label is not proof that its `two_stage` setting was immediately normalized to `union`. `src/controllers/gui/rag_preset_view_model.py:177-207` explicitly writes each preset key. Saved settings may differ from all these defaults.
8. `pipeline/repositories/memory_repo.py` supports `active|forgotten|all`, excluding deleted and island records on normal retrieval. History repository selects inactive, nondeleted SQLite history; its row identity differs from Timeline `EventId`. Source identity and actor attribution must be preserved, not reconstructed from text.

## Rust comparison at PR #276

| Capability | Python configured behavior | Current Rust generation | Acceptance gap |
|---|---|---|---|
| Admission | `RAG_ENABLED`, GameMaster and reaction-level policy | Explicit `history_generation Off/Keyword/Fts/Vector/Fusion`, captured per request; source retrieval depends on `KnowledgeRetrieval` | User-visible preset/settings mapping and operation-policy equivalence are unproven |
| Memory visibility | `all/active/forgotten` according to user setting; separate island behavior | Active Fact/RagOnly in joint semantic generation; islands/relations rendered separately | Authorized forgotten-scope integration and compatibility fixtures needed |
| History corpus | Stored `is_active=0`, not deleted, owner character | Archived subset of owner-scoped canonical `MitaView` with pinned `HistoryWindow` and original `EventId` | Different archival lifecycle; align behavior without a second writable source |
| Keyword | Only rows with missing legacy embedding in `keyword_only` bucket | Active snapshot lexical keyword matching; independent of embedding cache | Do not call these directly equivalent; derive a source-owned missing-vector view first |
| FTS | Source-specific SQLite FTS5 candidates for memory and history | Read-only derived History FTS; Memory selection in common generation currently lexical | Cross-source FTS mode candidate tests required |
| Vector | Optional; missing model allows enabled FTS/keyword buckets | One provider/model space, one query and document batch across both admitted sources; vector errors fail closed | Explicit mode-dependent unavailable-provider policy not accepted |
| Fusion | `union`, `vector_only`, `intersect`, `two_stage`, opt-in RRF; candidate membership varies | Preserve configured Memory admission where representable, then globally RRF Memory+History; History Vector/Fusion independent | Python all-source admission parity not established |
| Reranker | Linear feature rank then optional CE; CE failure retains base rank | One optional strict cross-source bounded reranker; errors and malformed scores fail closed | Adopt only an explicit compatibility mode after acceptance decision |
| Query/metadata | Expanded recent tail, lemmatization, actors, source templates | Canonical request query and original EventId evidence renderer | Per-source actor/templates/morphology fixtures missing |
| Documents/sentences | Sentence-level vectors optional; current Python RAG manager exposes memory/history/graph candidates, not an evidenced `DocumentRepository` | No canonical Document source owner, ingestion/indexing or text/visibility attribution | **R6 remains OPEN**: owner contract before query/index implementation |

## Executable acceptance next

1. Add deterministic mode matrix for the exact effective preset/config *after* settings application, covering independent `vector`, `keyword_only`, `fts`, `graph`, and `RRF` selection. Assert exact source-qualified ID sets, not only rank scores.
2. Prove explicit search while automatic RAG is disabled; verify GameMaster, `react_level=1`, `all/forgotten`, missing-model and user-changed settings separately. If Rust intentionally diverges, record and test that policy instead of silently claiming parity.
3. Implement only an owner-approved Documents admission contract: document ID/version, character/branch visibility, deleted state, original-file citation and canonical write owner; derived embeddings cannot own document data. A `RetrievalSource::Document` enum variant alone is not evidence of R6.
4. Keep Agent B's prepared-history atomic owner protocol as a hard dependency for D4; independent concatenated import operations cannot satisfy cross-domain crash atomicity.
5. Cycle36 integration adds semantic React evidence and mid-rerank Timeline cancellation, and malformed query/document embedding plus reranker score fail-closed fixtures. These are **R5 engineering expansions**, not proof of complete Python parity.

## Formal conclusion

**R5 PARTIAL.** Python mode defaults/presets, all-source candidate admission, missing-vector semantics, history lifecycle, CE failure policy, templates, and Documents/Sentence paths are not equivalent. Do not enable Python-like default RAG or introduce a silent fallback based solely on this static audit. **R6 OPEN, D4 PARTIAL**; no formal acceptance slot has changed in Cycle36.

## Cycle37 — source-qualified FTS engineering delta

- On the original stacked PR #280 baseline, `HistoryGenerationMode::Fts` previously used `Fts`/BM25 for archived Timeline History but `Keyword`/token intersection for Memory. The latter was **not** FTS equivalence even though it reached the same generation selector.
- Cycle37 introduces one ephemeral `scoped_fts::rank` engine for *both* already-authorized source slices. The History result keeps `History(EventId)`, branch and window metadata; Memory uses `Memory(MemoryId)` with only active `Fact|RagOnly` from the captured caller-owned snapshot. The buckets remain independently ranked `Fts` candidates and are combined by one global RRF/result budget. `unicode61` stemming/diacritics are tested directly; no keyword fallback exists on FTS errors.
- Bounds: 1024 indexed rows **per source**, no persistent table or write path. `HistorySearchError::{FtsUnavailable,FtsFailure,CorpusLimit}` remains a stable public contract. Memory-side failures are `GenerationFusionError::Fts`, before the model call. History generation setting remains opt-in and defaults to `Off`.
- This does **not** port Python's `keyword_only` missing-embedding admission, user-selected preset transforms, automatic/explicit RAG gating, CE fallback, forgotten scope, or query-builder morphology. In particular `LlmConversation::memory_query` includes a recent-history tail, so comparing complete prompts while assuming the user input is the *only* query term is invalid; deterministic mode comparisons must pin the **effective query** and authorized source sets. R5 is still PARTIAL.

## Cycle38 — Generation Keyword/RagOnly admission matrix

- Rust Off: ordinary `Fact`-only lexical shortlist (no archived History evidence), unchanged. Explicit Keyword: both Active `Fact` and `RagOnly` records with overlapping tokens are eligible alongside authorized archived History, through distinct `MemoryId`/`EventId` buckets. Explicit FTS: already admits the same active memory kinds through the shared temporary FTS5 engine. Semantic Vector/Fusion: both active kinds are vector candidates; Keyword sub-buckets now also include matching RagOnly. Intersect 2-method admission requires the actual query-matched memory Keyword key, not mere presence in the vector corpus.
- The tested `LlmConversation::memory_query` includes recent Timeline text and can match unrelated common words. Mode/admission fixtures therefore use deliberately disjoint controls and validate exact evidence identities under the same observed query-generation mechanism, not an assumed user-only query.
- This closes the Rust-only RagOnly omission in Keyword and Intersect. It does **not** establish the Python `keyword_only` missing-embedding condition or its preset/automatic RAG permissions, and does not authorize introducing a second missing-vector source flag. R5 formal status remains PARTIAL.
