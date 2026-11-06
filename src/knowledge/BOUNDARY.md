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
