# Knowledge and AI execution boundaries

- **Timeline** owns authoritative memory records and their identities. Captured `CharacterContext` defines permitted membership; the derived vector cache cannot add records.
- **Knowledge** (`knowledge/`) selects from the supplied snapshot using pure lexical ranking, cosine similarity of provider-returned vectors, and source-keyed fusion. The embedding port is defined here; it owns no model, worker process, model installation, credentials or GPU lifecycle.
- **AI adapters** (`adapters/`) implement external inference protocols. `GeminiEmbeddingProvider` lives here; an AI Engine IPC embedding adapter would also live outside the knowledge domain.
- **Request orchestration** (`request::MessageHandler`) chooses the configured KnowledgeRetrieval mode, awaits selection, and supplies the bounded prompt payload to the provider-neutral `GenerationRequest`. Cancellation and stale selection checks cover retrieval and generation as a single operation.
- **Generation** (`generation/`) dispatches provider-specific LLM requests and parses structured responses. `GeminiProvider` is one member of the `GenerationProvider` registry, not the shared generation engine. Provider-specific code never invokes retrieval.

Failures from semantic retrieval are explicit errors; the selected mode cannot silently fall back to lexical. Lexical mode is the explicit default until an embedding adapter is configured. No model runtime is linked into Core by this boundary refactor.

## Source-scoped Knowledge Graph extraction (partial #23)

- `GraphExtractor` produces a bounded, validated proposal for a captured `MemoryRecord` and its revision; extraction alone cannot mutate user data.
- The character owner commits accepted source-scoped relations as canonical Timeline memory changes. Graph queries rebuild from that Timeline-derived projection; no separate mutable graph database is introduced.
- A changed, forgotten, deleted, expired or subsequently restored source cannot resurrect an earlier extraction. Stale extractor results are rejected at the character edit boundary. Reprocessing replaces the prior relations for the same source; an empty extraction explicitly clears them.
- `query_extracted_graph` provides bounded, deterministic lexical retrieval of extracted relations with source identity and source revision. Historic manual `LinkEntities` events remain replay-compatible but cannot claim modern source provenance.
- `propose_inline_relations` accepts the active Python structured-response `subject|predicate|object` field format only when the caller already owns a captured, source-versioned memory. The entire input is validated before any Timeline append; it does not auto-associate conversation-only graph triples with an arbitrary memory. Connecting this parser to the real structured generation accept flow is still pending. Python source reference: `Atm4x/NeuroMita` `5f3d061f523d9abefa6a264bb2953048a935fa65`, `src/schemas/structured_response.py` and `src/controllers/model_controller.py`.
- Further work is needed for live extraction adapter wiring, non-memory event sources, richer identity resolution, user-facing inspection/reindex/clear workflows and Python behavior parity. This is deliberately **not** full #23 acceptance.

## Graph maintenance (partial #50)

- `preview_graph_maintenance` takes an immutable character memory snapshot, exact unique source IDs and an injected clock value. It exposes each source revision and existing sourced relations without writing anything.
- `apply_graph_maintenance` requires explicit confirmation. `Clear` removes only the selected sources' derived relations; `Reindex` awaits an explicitly supplied extractor. No extractor means an explicit unavailable-backend error, not a fake lexical replacement.
- Extractor failure, cancellation before commit and stale memory snapshots reject the **entire** proposed batch. The character owner validates the original Timeline revision and commits all resulting source updates in a single append. Cancellation is intentionally not promised after the write is dispatched.
- This is not a generic backup/restore or settings migration feature. Graph UI controls, embedding cache inspection, other maintenance domains, archive validation and full Python parity remain open.

## Derived embedding-cache maintenance (partial #50)

- `SemanticMemoryRetriever::preview_cache_clear` reads only counts, character scopes, model-space identity and an opaque instance revision; it never exposes memory text or vector contents. `clear_cache_confirmed` requires explicit confirmation and the exact current preview, rejecting stale and foreign previews.
- The clear invalidation epoch prevents in-flight document embedding batches started before a clear from repopulating the cache. Their original retrieval result may still complete for their captured read-only source snapshot; no canonical Timeline state changes. Cache reindex remains lazy on subsequent retrieval.
- The direct `clear_cache` method remains an internal invalidation mechanism for non-UI callers; presentation adapters must use the confirmed API. This is only the derived in-memory cache, not a persistent vector index or universal data maintenance UI.

## Explicit semantic-memory + source-graph fusion (partial #22/#23)

- `knowledge::graph_retrieval::retrieve_memory_with_graph` is an opt-in read adapter over a caller-supplied character projection and injected as-of timestamp. It queries only versioned memory-derived graph relations and the existing bounded semantic memory shortlist; no graph DB, extra embedding backend or hidden fallback is introduced.
- The typed result contains separately identified `Memory` and `Graph` retrieval hits, distinct `Vector`/`Graph` evidence, source memory ID/revision metadata, embedding cost, and provider space. `FusionMode` explicitly determines inclusion; `VectorOnly` excludes graph evidence. Semantic provider errors remain errors, even if graph evidence is available.
- Graph source expiry/update/delete and fork/historical selection are governed by the captured `CharacterMemories` projection. The adapter does not automatically wire graph candidates to normal generation, does not infer a character identity from arbitrary data, and does not provide history/document/FTS retrieval. Caller ownership of the snapshot remains mandatory.

## Explicit recalled-forgotten selection (partial #22)

- `KnowledgeRetrieval::select_recalled_forgotten` accepts an explicitly selected `ForgottenRecall` and the **same character's captured `CharacterMemorySnapshot`**. The character, Timeline branch/prefix, source IDs, source contents and Forgotten/Fact state must match before retrieval or any embedding calls. Stale, foreign, duplicate or altered evidence fails closed, without partially selected output. The async method now returns opaque `PendingHistoricalRecall`, **not prompt text**: a caller must obtain a *fresh* character-owned Timeline snapshot after retrieval and consume it using `finalize(&fresh_snapshot)`. Exact branch, Timeline position, context reset epoch, memory revisions and content are rechecked before revealing text. Any drift is `StaleRecall`, including unrelated Timeline head changes; successful revalidation does not itself grant user consent.
- Both lexical and semantic modes operate **only on the previously recalled IDs**, never on the entire forgotten corpus. The existing 12-record/6000-character prompt budget and `[FORGOTTEN]` marker are retained. Provider errors remain errors; no lexical fallback is added.
- This method is **not** an authorization or user-consent system. A request owner must first authorize the explicit recall action, capture the character's current snapshot and ensure it remains current before using any returned text. This method is **not wired** into `MessageHandler` or ordinary generation, which remains active-only.
- `SemanticMemoryRetriever::retrieve_scoped` continues to expose three explicit snapshot-read modes for callers that already own and authorize the source; this narrower API intentionally cannot broaden a selected recall into unrelated forgotten facts or `RagOnly` entries. Full request UI, permission policy and other RAG sources remain unimplemented.

## Live-owner explicit forgotten recall (partial #22)

- `app::historical_recall::recall_from_character(&KnowledgeRetrieval, &CharacterHandle, RecallSelector, query)` is a **read-only, explicitly invoked** bridge from a character-owned Timeline projection to the existing historical selection. It acquires `CharacterHandle::memories()` itself before choosing records, and re-reads the same owner after awaited semantic retrieval before finalizing the pending text. The caller cannot accidentally finalize against its own stale cached snapshot through this API.
- The direct immutable-slice selector `recall_forgotten_records` shares the same validation/ranking implementation as the existing `recall_forgotten(&CharacterMemories, ...)` public pure helper. It does not reconstruct or own an alternate `CharacterMemories` store and never changes Timeline, memory states or source revisions.
- A missing, duplicate, active, deleted or foreign ID is a hard error, and the owner is re-read only after a successful historical selection. Switching the branch, resetting the context or editing a source during embedding rejects the output as `StaleRecall`. The ordinary generation pipeline is unchanged and remains active-only.
- **This is not an access-control grant**: only an explicit user-facing action should call the method. There is still no GUI/ChatApi historical-recall command or proof that a user authorized it, and no atomic request-acceptance handshake. Do not wire this into automatic reply/React/Initiate generation or count it as full RAG parity.
