# R7 failure and provenance contract — Agent A Cycle39

**Acceptance status: OPEN.** R7 requires the full user-visible failure matrix, including missing/corrupt index, timeouts, partial sources, and model/provider failure. Passing this targeted provenance slice is not sufficient to mark R7 PARTIAL or PASS without the missing workflows.

## Canonical boundary

`MitaView` is the single owner of visible History. The retrieval result is a derived candidate, not a new authority. At final `render_history_evidence` / `render_archived_history_evidence` admission, the selected character, branch, Timeline prefix, archived `HistoryWindow`, original `EventId`, User/Assistant role, exact source metadata (`message_id`, `sequence`, `learned_at`, `role`, `branch_id`, `through`) and **exact Unicode character-budgeted content** are reconstructed from the current `MitaView`. The renderer also rejects Selected-vs-hit-count inconsistency, duplicate `EventId`, missing/duplicate/invalid retrieval methods and nonfinite scores. This avoids treating a valid prefix of a message as a verified complete quote or accepting metadata spoofing after fusion/reranking. A validation error is `HistoryEvidenceError::InvalidEvidence` or `StaleSelection`, not alternate retrieval.

`history_retrieval::hit` already owns the canonical metadata format; the renderer compares against it instead of introducing a second serialization contract. Legitimate search results truncated at the same `max_content_chars` budget remain valid. The renderer does not modify Timeline, change visibility, allocate a persistent index or perform model calls.

## Observed cases and executable proofs

| Case | Current behavior | Focused evidence |
|---|---|---|
| No archived source / no matches / selected | Distinct status; empty result contains no rendered evidence | `rag_history_source::history_source_has_real_provenance_and_distinct_zero_result_states` |
| Foreign character, branch or Timeline prefix | Explicit stale/invalid evidence | `rag_history_source::history_never_crosses_witness_or_message_visibility_boundaries`, `archived_history_scope_is_invalidated_by_revision_and_privacy` |
| Forged content or EventId / incorrect observer source | Rejected before generation evidence rendering | `rag_history_generation::generation_renderer_rejects_forged_content_and_cross_character_source_identity` |
| Forged `role`, `message_id`, `sequence`, `learned_at`, branch/through metadata, extra metadata, duplicated EventId | Exact metadata and ID membership checked; rejected | **Cycle39** `archived_history_renderer_rejects_partial_quotes_and_forged_provenance` |
| Prefix-only truncated quote or changed Unicode character budget | Must equal canonical excerpt for the requested budget | **Cycle39** `archived_history_renderer_requires_exact_unicode_character_budget` |
| Missing, invalid or duplicate evidence method; nonfinite retrieval score | Rejected, rather than inserted into LLM prompt | **Cycle39** `archived_history_renderer_rejects_partial_quotes_and_forged_provenance` |
| Missing SQLite FTS5 feature, invalid query/corpus bounds | Typed failure; no Keyword substitution | `rag_history_source::explicit_fts_mode_rejects_missing_backend_without_lexical_substitution`, `fts5_rejects_invalid_query_and_overlarge_corpus_before_building_index` |
| Embedding-space mismatch, invalid query/doc embedding or failed provider/reranker | Typed failure; no semantic→keyword substitution | `rag_history_vector::model_space_and_provider_failures_are_not_keyword_fallbacks_or_cache_entries`, `rag_history_generation::joint_generation_rejects_malformed_query_and_document_embeddings_before_llm`, `joint_fusion_one_budget_reranker_selects_history_over_memory_and_rejects_failed_provider` |
| Timeline revision during embedding/rerank | Discard selection; no LLM call | `rag_history_generation::changing_timeline_during_vector_search_cancels_before_provider_invocation`, `timeline_revision_change_during_joint_rerank_cancels_before_llm_admission` |

## Unclosed required R7 work

1. **Documents R6** has no accepted owner/index/version/citation boundary, so corrupt/missing document-index, rebuild, partial document batch and recovery cannot be tested end to end. Do not emulate a persistent document index from ephemeral History FTS5.
2. **Timeouts and cancellation** of real Python embedding/reranker/model workers need a bounded, configured deadline and user-facing error/outcome verification. Paused test adapters and Timeline cancellation alone do not prove actual network/worker timeout UX.
3. **Partial multi-source failures** require an explicit all-or-nothing/degraded-results policy accepted by the user. Do not invent lexical fallbacks when one semantic source fails.
4. **Real stock GUI/CLI failure presentation** and recovery/retry after process crash/provider restart, including corrupted derived state, are not accepted by unit/integration tests or `cargo check` alone.

The current implementation remains a strict, local, source-owned retrieval engine with **no parallel mutable Memory/History index**, no implicit mode change and no unrelated Legacy modifications.
