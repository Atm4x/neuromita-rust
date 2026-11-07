# Knowledge/RAG Python parity check — 2026-11-06

Reference checkout: `Atm4x/NeuroMita`, `bridge/session-timeline-reexport-20261103`, SHA `5f3d061f523d9abefa6a264bb2953048a935fa65`. Retrieved through GitHub Actions `sandbox-python-source-20261103`, artifact ID `13199870767`; Git bundle SHA-256 `6002982b303cb2fbbc21e6deb551804b8c2c0c498fdd38e0d4bbaf52ff116435`; full Git bundle verified. Only observed source paths below are used as evidence, not every old helper or doc in that repository.

| Active Python path | Observed behavior | Rust status / difference |
| --- | --- | --- |
| `src/schemas/structured_response.py` (`relations`) and `src/controllers/model_controller.py` (inline graph payload) | Structured response may contain `subject|predicate|object` strings; graph extraction can receive these in the same generation turn without a separate model invocation. | `knowledge::graph::propose_inline_relations` now strictly parses complete arrays into a source-versioned memory proposal, for an **explicitly supplied canonical memory source**. It is *not yet* connected to the production structured-response acceptance path. Rust `StructuredResponse::ensure_supported_effects` currently rejects `entities` / `relations`, so no live parity is claimed. Conversation-only relations cannot be attached to an arbitrary memory ID. |
| `src/controllers/graph_controller.py` (`MESSAGE_COMPLETED`) | Depending on settings, graph ingestion uses inline structured data or dedicated extraction; associated message IDs may be tracked. | Memory-derived graph extraction, canonical provenance and deterministic query exist. Non-memory Timeline event provenance and live generation wiring remain open. |
| `src/managers/rag/rag_manager.py` (`search_relevant`) and `src/managers/rag/pipeline/config.py` (`memory_mode`) | Configured `RAG_MEMORY_MODE` can be `all`, `active`, or `forgotten`, with a default of `all` at this source SHA. RAG may retrieve retained forgotten memories. | Ordinary Rust generation continues to exclude forgotten memories. `SemanticMemoryRetriever::retrieve_scoped` now exposes a read-only, opt-in `MemoryRetrievalScope::{ActiveOnly, ActiveAndForgotten, ForgottenOnly}` over the caller-provided canonical `CharacterMemories` projection. Its as-of cutoff excludes expired active rows; deleted and island records are excluded in all modes. This does **not** change the default or wire forgotten retrieval to the production prompt. User authorization and request policy binding remain separate. |
| `src/managers/rag/pipeline/repositories/memory_repo.py` | Per-character memory queries filter deleted/islands and optionally remembered/forgotten state; vector/FTS/keyword sources are distinct. | Rust performs a captured Timeline memory-only vector/lexical fusion. Model-space validation, cache clear fencing and explicit no-fallback errors exist; history/FTS/graph source fusion and live-model parity remain open. |
| `src/managers/rag/graph/graph_store.py` (`clear_for_character`) | Per-character graph cleanup deletes relations/entities/processed messages from the legacy graph SQLite tables. | Rust owns source-versioned graph relationships in Timeline. The current confirmed graph maintenance API clears selected **memory-derived** relations; there is no full per-character graph reset or entity store to purge. Historic manual links remain a separate canonical event type and must not be silently deleted by memory-only maintenance. |
| `src/managers/rag/rag_manager.py` (`search_relevant`) | Partial retriever failures can produce empty buckets; cross-encoder failure can continue with previous scores. | Current Rust semantic provider and optional reranker are intentionally fail-closed. Compatibility policy must be explicit per chosen user mode, not an invisible fallback. |

Source format verification does not prove end-to-end Rust behavior. Prior to closing #21–#23, test each implemented path with an active reference fixture and the exact requested mode. This document is a gap register, not a migration schema or an implementation promise for other agents' domains.

## Explicit semantic forgotten-memory search (2026-11-07)

The opt-in `SemanticMemoryRetriever::retrieve_scoped(character, snapshot, query, as_of_unix_ms, scope, filter, fusion)` accepts only a captured, character-owned `CharacterMemories` projection. It can select currently available active facts, explicitly forgotten facts (including RAG-only sources), or both. `MemoryState::Deleted` and typed islands are always excluded. A still-active record whose `expires_at_unix_ms` is at/before `as_of_unix_ms` is never selected; forgotten records are retained history rather than time-active prompt material. The search never changes state, restores a record, or writes Timeline.

Old `retrieve`, `retrieve_filtered` and `retrieve_with_fusion` retain their active-only behavior and signatures. The new method requires explicit `MemoryRetrievalScope`; it reuses the exact existing embedding/cache/fusion machinery, model-space validation, provider error semantics, and prompt budget instead of introducing a second index. Historical/forked membership follows the captured Timeline projection. Returned text from `ActiveAndForgotten` or `ForgottenOnly` **must not be inserted into an ordinary generation prompt** unless an authorized request owner explicitly selected that scope. The actual Python default `RAG_MEMORY_MODE=all` is **not** silently enabled in Rust. There is still no production binding from a user RAG mode setting to the new API.

## History keyword source (2026-11-07, Agent A cycle 16)

Python runtime at `Atm4x/NeuroMita` `5f3d061f523d9abefa6a264bb2953048a935fa65`:
`src/managers/rag/rag_manager.py` instantiates `HistoryRepository`, reads recent
active history for query expansion, and the RAG search pipeline can query old
(`is_active=0`, non-deleted) history via distinct keyword, SQLite FTS5 and vector
repositories (`src/managers/rag/pipeline/repositories/history_repo.py`). Settings
`RAG_SEARCH_HISTORY=True` and `RAG_USE_FTS=True` are configured defaults, with
`RAG_VECTOR_SEARCH_ENABLED=False` in `pipeline/config.py`. Thus history keyword
search is an active source, **not** an obsolete experiment.

Rust now offers an explicit, locally inspectable history **keyword** path:
`knowledge::history_retrieval::search_history` consumes the canonical
`TimelineMessageQueries::capture`/`MitaView` for one character and fixed branch
prefix. It uses the current message projection, respecting seen_by, reset,
MessageHidden and MessageRevised; it never treats an observed retelling as
firsthand knowledge. It returns bounded `RetrievalSource::History` evidence,
canonical EventId, message role and Timeline provenance, with distinct
`EmptyCorpus` and `NoMatches` states. Invalid queries and foreign-character
views fail explicitly. This source has no independent database/index.

`HistorySearchApi::inspect` is wired through the live CharacterHandle and
checks the selected owner again before exposing results. The interactive
terminal command `/rag history QUERY` is a **local explicit read-only action**,
not automatic prompt injection, and is not authenticated for remote use.
`tests/rag_history_source.rs` covers in-memory, SQLite reopen, visibility,
revision, reset, status and rejection; `tests/rag_history_api.rs` runs the
real app/ChatApi/owner read path. Unknown slash commands cannot silently
become LLM inputs.

**Still open:** automatic user-configured history-source composition in
Reply/React/Initiate; FTS5 query mode and ranking, vector-history embedding
and cache reuse, documents and sentence-level retrieval, model-switch/index
recovery, reranking across all sources and strict Python output parity.
The default Rust generation policy is unchanged. Neither the new history
keyword adapter nor test pass is evidence of full issue #22 acceptance.
