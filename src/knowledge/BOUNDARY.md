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
- **This is not an access-control grant**: only an explicit user-facing action should call the method. The local terminal (and the dedicated Slint Memory editor action) can explicitly invoke read-only inspection, but neither grants generic remote API permission or an atomic prompt-acceptance authorization handshake. Do not wire this into automatic reply/React/Initiate generation or count it as full RAG parity.

## Explicit console historical inspection (partial #22)

- The interactive CLI `/recall forgotten id UUID | QUERY` or `/recall forgotten search | QUERY` is an **explicit local stdin action**, never a Reply/React/Initiate message. It invokes `HistoricalRecallApi::inspect`, which resolves the already-running app owner and existing `KnowledgeRetrieval` instance. No extra index, cache, settings store or provider is created.
- It returns read-only `[FORGOTTEN]` text only after `app::historical_recall` rechecks the live Timeline owner following the awaited retrieval. Empty results and invalid selectors are reported directly to the console; malformed slash commands are not forwarded as chat.
- CLI stdin is trusted as an **interactive local user surface**, but `HistoricalRecallApi::inspect` does **not** independently authenticate an arbitrary caller. Remote services and plugins must enforce their own access control before calling it. No forgotten memory is automatically injected into prompts, and accepted-turn atomic authorization remains out of scope.

## Explicit Slint Memory inspection (partial #22)

- Under Settings → Characters → Character history and memory, the Slint editor exposes **Inspect forgotten** only for a selected `Forgotten` `Fact` and a nonempty query. The action is separate from Add/Update/Forget/Restore and never submits a chat message. It dispatches `MemoryRecall` with a unique request identity to the existing running `HistoricalRecallApi::inspect` and returns only display text.
- The desktop runtime executes each request asynchronously with shutdown cancellation. The UI discards a delayed answer after changing the selected record/character or when its original request identity no longer matches, and clears displayed historical text after a changed Timeline memory snapshot. Backend selection and post-embedding freshness checks remain authoritative; UI filtering is additional isolation, not a replacement.
- The action is local desktop user input, not authenticated remote access. It does not prove permission to insert forgotten information into a later model prompt; automatic Reply/React/Initiate remains active-only.

## Explicit semantic History inspection (partial #22; Agent A cycle 17)

- `HistorySearchApi::inspect_semantic` is a local, explicit read-only operation
  using the **same** running `KnowledgeRetrieval::Semantic` provider and bounded
  derived cache as character memory. Its mode is explicit (`VectorOnly` or
  lexical/vector `Union`); `Lexical` configuration returns typed
  `BackendUnavailable`, **not** fallback keyword candidates. Normal generation
  remains unchanged and cannot see this inspection result.
- Every History cache record is keyed by character, `History(EventId)` vs
  `Memory(MemoryId)`, excerpt content and full embedding space. Projection
  membership is always recomputed from the caller's `MitaView`; reset, hide,
  revision, branch/fork and per-character privacy are enforced at read time.
  Embeddings from a different model/protocol/backend, even with equal dimensions,
  are rejected as `ModelSpaceMismatch` before entering the cache.
- `HistorySearchApi` captures a live CharacterHandle Timeline anchor before
  awaits and re-reads the owner after retrieval. Stale results are never
  returned to a local caller. There is no HTTP/plugin exposure or permission
  grant to include this history in model prompts; a future remote/accepted-turn
  path needs authenticated, scoped permissions and acceptance-time validation.
- History vector scans a bounded current corpus (1024 visible messages,
  first 4000 searchable characters per message). Over-capacity is an explicit
  error, not a hidden source fallback. An absent derived cache row is a cache
  miss and is recomputed, **not** a missing persistent index failure.

## Local confirmed cache-clear workflow (partial #50; Agent A cycle 18)

- `KnowledgeCacheApi::preview` reads the **running** `KnowledgeRetrieval` owner, returns a bounded `EmbeddingCachePreview` (model-space, total count and per-character counts) without message text or vectors. In lexical-only mode it returns `BackendUnavailable`, rather than constructing an embedding model for inspection.
- `KnowledgeCacheApi::clear_confirmed` delegates to the **same** owner and requires a preview plus explicit confirmation. The cache instance/revision guard rejects stale or foreign previews atomically; clearing is confined to derived memory/history embeddings, never Timeline or the canonical memory store. In-flight batches started before the clear cannot repopulate the cache after invalidation.
- The interactive terminal command `/knowledge cache preview` issues an opaque one-use UUID confirmation token stored **only within the running terminal session**. `/knowledge cache clear TOKEN` requires an exact matching token. Unknown commands and malformed token formats fail closed and do not become model chat. A failed/consumed confirmation requires another preview; a wrong token does not consume the valid pending preview. Changing characters does not change the scope: the preview explicitly lists each character's entry count and confirmation clears the shared derived cache across all character scopes.
- This is a local trusted-user workflow; no unauthenticated remote route, permanent confirmation token or background cleanup task was added. The entire #50 issue still includes graph maintenance, other data operations and cross-agent import/export acceptance.

## Explicit in-memory SQLite FTS5 over Timeline History (partial #22; Agent A cycle 18)

- `knowledge::history_retrieval::search_history_fts` builds a **temporary** SQLite FTS5 `unicode61` table exclusively from one authorized `MitaView` of the selected character. It indexes at most 1024 visible messages and at most 4000 Unicode characters per message, then disposes of the table at the end of the read. There is no persistent index to repair, no second writable canonical store, no asynchronous backend, and no migration. The current view (not previous FTS matches) controls History membership. An over-capacity view returns `CorpusLimit` instead of silently truncating the source corpus.
- A sanitized, literal quoted OR query (derived from the same validation tokenizer as the keyword path) is bound as a parameter; user text cannot inject FTS operators or SQL statements. Results are ranked by SQLite `bm25`, then provided with canonical EventId, role, branch, Timeline prefix, and source-scoped `Fts` evidence. `EmptyCorpus`, `NoMatches`, `FtsUnavailable`, and typed FTS failures are distinct; there is **no** fallback to keyword when FTS5 is unavailable. The operation requires existing `sqlite-system` feature; disabled builds return `FtsUnavailable`.
- `HistorySearchApi::inspect_fts` obtains and revalidates the live CharacterHandle's owner snapshot; the CLI exposes `/rag history-fts QUERY` as a **local explicit read-only action**. It never modifies Timeline or enters normal Reply/React/Initiate prompts. This is not Python parity for the older inactive-history window, query-tail expansion, morphology, documents, runtime default settings, or persistent FTS5 indexes.

## Opt-in History RAG in production requests (Agent A cycle 19)

- `SettingsApi::snapshot::<HistoryGenerationSettings>` and the trusted-local `/rag generation [off|keyword|fts|vector|fusion]` command expose an explicit, persisted user preference. The default is `off` (no behavior change). `MessageHandler` uses its already **admitted** `SettingsSnapshot` instead of reading mutable settings after enqueue. The limited query, top-k and characters-per-hit budgets are validated by typed settings and retrieval contracts. Invalid settings cannot be persisted.
- For opted-in Reply/React/Initiate, the request owner invokes the existing keyword/SQLite-FTS5/vector/fusion selectors **on the already captured character-owned `MitaView`**; a selected source is never switched to another backend when it fails. The resulting `[RETRIEVED TIMELINE EVIDENCE]` block uses quoted JSON strings, source EventIds, a data-only warning and a bounded hit count. The rendering operation rejects foreign/missing/changed evidence, branch/prefix mismatches and oversized entries. No forgotten/deleted memory is included; there is no implicit historical recall or new storage.
- The async Request path re-reads the authoritative character owner after retrieval and rejects a changed Timeline prefix before calling the provider. With history generation opted in, the canonical Character acceptance routine also **atomically** compares the captured exact branch+through against the current Timeline head while holding its Timeline lock, before any effects or commit. A context reset, delete/hide/edit or newly appended event after capture therefore cannot produce an accepted turn using stale selected History. With `off`, the existing less-restrictive ordinary generation acceptance rules remain unchanged.
- The application still sends the whole allowed `MitaView` as its regular conversational history. This new opt-in operation **prioritizes retrieved evidence from within that same view**; it does **not** recreate Python's `is_active=0` old-history window, evict older messages from the conversation, recover unavailable documents, implement morphology or complete generation reranking. This limitation is intentional and must not be mistaken for full #22 Python parity. Future History windowing will need one owner-defined active/archived selection contract, not a second database of conversation facts.
- `HistoryGenerationSettings` is app-level and persisted through existing settings owner; it does not authenticate remote callers. Only local user surfaces should change it until authenticated per-client permission scopes exist. The model sees the evidence solely as non-authoritative data; source text is never an instruction grant.

## Admission-pinned active History window and archived-only RAG (Agent A cycle 20)

- Only with `HistoryGenerationMode != Off`, one `HistoryWindow::capture` splits the *same* immutable, character-owned `MitaView` into the most recent 1–64 eligible User/Assistant messages (default eight) and the older excluded messages. The setting is persisted and captured at request admission. The trusted local CLI exposes `/rag generation window N`; it affects only subsequent requests. The default `Off` path retains the full known conversation, byte-for-byte under the existing projector.
- An explicitly selected older Reply/React message is always kept active, together with newer messages, preserving the causal focus; it is **not** duplicated in archival evidence. System messages and World/Game entries remain in normal conversation instead of being placed in History retrieval. The split is based on acquisition order `learned_at`, not a second mutable active-state flag or SQL table.
- The same typed window is used to select candidates for keyword, FTS5/BM25 and vector/fusion **before** scoring/embedding. Standard OpenAI and Gemini adapters both apply the active filter in their common provider-neutral conversation projector; old excluded messages are absent from ordinary provider prompt and can only appear as explicitly selected, provenance-checked historical evidence. An old message cannot simultaneously be serialized as active and retrieved as archival evidence.
- `render_archived_history_evidence` rejects active-source IDs, foreign character IDs, mismatched text and stale Timeline window identity. The existing request-time post-await freshness check and exact prefix validation under the canonical Character acceptance lock remain in force. Neither the split nor the prompt rendering owns/updates Timeline or memories; deleted/forgotten and unwitnessed messages remain unavailable.
- **Python parity is still partial**: the Python reference uses its `history.is_active=0` rows while this Rust implementation derives an explicitly configurable rolling window from the live Timeline view, with no independent historical status mutation. Document repositories, stemming, old-history FTS persistence, full cross-source reranking and GUI workflow are not complete. A custom external GenerationProvider must respect the typed `GenerationInput.history_window` just as the built-in adapters do; that in-process port is trusted, not an untrusted HTTP boundary.

## Trusted-local graph maintenance workflow (Agent A cycle 21; partial #50)

- `GraphMaintenanceApi::preview` uses the running `CharacterHandle::memories` snapshot and the existing pure `preview_graph_maintenance` validator. It previews 1–256 unique, exact `MemoryId` sources belonging to the selected character, their current source revisions and only their derived relation counts; it receives a caller-injected `as_of_unix_ms` for TTL consistency. There is no second graph repository or new storage schema.
- `/knowledge graph preview clear MEMORY_UUID...` shows the exact selection and a random one-use session-local confirmation token; `/knowledge graph apply TOKEN` consumes it and calls `GraphMaintenanceApi::apply_confirmed`. A wrong token or character cannot clear records, and switching character discards the pending token. `clear` replaces only extracted graph relations of selected sources with the empty set through the **canonical atomic Character memory-edit boundary**. Canonical facts and other characters remain unchanged. Stale previews after changes/reset cannot commit; error and cancellation before commit are explicit.
- `/knowledge graph preview reindex MEMORY_UUID...` can inspect the existing source/scope, but without a configured production `GraphExtractor`, confirmed `apply` returns `BackendUnavailable` and leaves the Timeline unchanged. The application does **not** fabricate relations with a lexical fallback. A separate authenticated/owner-scoped extractor binding is required before production reindex can be advertised as available. The underlying pure `apply_graph_maintenance` supports an explicitly injected extractor and has unit tests for provider failures, cancellation, batch atomicity and SQLite reopen.
- The Slint Memory Editor now exposes **Inspect graph relations → Confirm clear** for exactly one selected active `Fact`/`RagOnly` memory. Its `GraphPreview`/`GraphApply` desktop commands use the running `GraphMaintenanceApi` with a private plan retained in the desktop runtime, never sent as an editable UI payload. It displays only the source relation count, not a generated assertion of success. The UI gates responses by a per-preview UUID, exact selected MemoryId and selected character; selection changes or refreshed Timeline snapshots invalidate unconfirmed plans. A wrong or stale response is not displayed. An explicit Cancel discards an unconfirmed plan; cancellation is not offered **after** the confirmed apply has been dispatched, because owner acceptance may already have committed. After `GraphApply` returns, success/errors are rendered only for the matching pending UI request. Core still independently enforces character, revision and atomicity.
- Reindex is **not offered in the stock UI** because the default launcher has no configured `GraphExtractor`. In an explicitly configured application, the same Slint panel gates Reindex on the running application capability. `StructuredResponse::ensure_supported_effects` currently rejects `entities/relations`; do not bypass it through raw text, a fabricated heuristic extractor or implicit fallback. The existing local CLI shows `BackendUnavailable` when reindex is attempted without a provider. GUI callback tests currently compile (`cargo check --tests`) but need executable GUI acceptance when linking resources permit.
- This remains a **local trusted-user** workflow; `GraphMaintenanceApi` does not authenticate remote callers and must not be exposed through a public HTTP/plugin route without scoped authorization. General backups/imports, full reindex ingestion and broad #50 acceptance remain separate tasks.

## Explicit GraphExtractor wiring (Agent A cycle 23)

- `ApplicationConfig::with_graph_extractor(Arc<dyn GraphExtractor>)` is the **single composition-root injection point** for Graph Reindex. The optional, immutable selected backend is retained by the running `ApiBindings`; it is not a global mutable registry, a model fallback, or a second graph store. By default the application installs **no** extractor. `GraphMaintenanceApi::reindex_available()` reports only whether the running composition has an injected adapter; it does not assert that a remote worker is currently healthy. Provider outages must be reported by the adapter as `GraphError::Provider`, never converted into synthetic relations.
- `GraphMaintenanceApi::apply_confirmed` reuses exactly the same `CharacterHandle`, source-revisioned preview, `GraphExtractor` and atomic `apply_graph_maintenance` as the pure domain path. The target source cannot change unnoticed after an awaited provider call: failed validation, failed extraction or pre-commit cancellation produces **no partial relation writes**. The extractor is called only after explicit confirmation. Identical reindex proposals remain idempotent, and canonical facts and other characters are unaffected.
- A trusted local CLI exposes both `/knowledge graph preview reindex MEMORY_UUID...` and `/knowledge graph apply TOKEN` **only when an extractor has been configured**; without one, preview remains inspectable but no misleading confirmation token is offered. The Slint surface supports confirmed Clear unconditionally and confirmed Reindex **only when the running application has a configured extractor**; the default launcher still has none. Supplying a mock `GraphExtractor` in tests demonstrates the actual live API/Timeline path, **not** production AI-model parity.
- A future AI/Generation-owned adapter must supply an explicitly chosen and admission-stable model/preset and a typed, bounded `Vec<MemoryRelation>` parsed with its own extraction-only response schema. It must not call `StructuredResponse::ensure_supported_effects` with unauthorized `entities/relations`, bypass the standard reply validation, or silently switch to keyword extraction. The existing canonical `propose_extraction` and owner acceptance remain the only graph-write route. Until that adapter is deployed in the default launcher, stock Reindex remains unavailable and broad #23/#50 acceptance is open.

## Capability-gated Slint Graph Reindex workflow (Agent A cycle 24)

- The desktop publishes `GraphCapability` from the running `GraphMaintenanceApi::reindex_available()` only after application startup and resets it on shutdown. This is a *composition capability*, not proof of provider health or extraction quality. Stock GUI startup has no adapter, so no Reindex button is offered and direct callback attempts fail closed.
- The two separate actions, **Inspect Clear** and **Inspect Reindex**, dispatch the same typed `GraphPreview(request_uuid, character, MemoryId, mode)` command. The desktop captures the canonical immutable `GraphMaintenancePreview` and retains it privately, never exposing an editable plan in Slint. A separate confirmation dispatches the already-previewed operation; mode cannot be changed between preview and acceptance. Provider errors remain visible, with no lexical substitute and no fake success.
- A late preview, foreign selected MemoryId, character switch, revised Timeline snapshot or removed capability invalidates an **unconfirmed** plan; confirmed apply remains non-cancellable at the UI once dispatched. The canonical owner validates the source and accepts the entire reindex batch atomically. GUI binding tests cover disabled capability, exact mode, reindex preview/confirm/apply, late backend loss and old Clear compatibility.
- The only **production** backend still missing is the AI-owned extraction adapter that explicitly pins character/model/preset at admission and validates the extraction-specific schema. This UI integration does not claim the model worker is present, that it is healthy or that #23/#50 macro acceptance is closed.

## Graph relation provenance inspection (Agent A cycle 25)

The existing versioned `GraphMaintenancePreview` exposes a derived, read-only
`inspected_relations()` projection that pairs each relation with its canonical
`MemoryId` and source revision. The trusted-local `/knowledge graph inspect ID...`
command never creates a confirmation token or mutates Timeline. The selected
Slint Memory Editor displays these source-anchored triples from **the same**
preview used by confirmed Clear/Reindex. Values are JSON-quoted for unambiguous
literal display. Pending selection UUIDs and source/character guards remain in
place; successful mutation invalidates the shown data. The default GUI still
has no model-backed GraphExtractor and cannot advertise production Reindex.

The explicit 26-slot, non-official acceptance rubric is documented in
[`ACCEPTANCE_MATRIX.md`](ACCEPTANCE_MATRIX.md). No macro completion is inferred
from adding this inspection surface alone.
