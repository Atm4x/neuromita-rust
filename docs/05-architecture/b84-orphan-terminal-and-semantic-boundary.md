# B84 — Explicit orphan terminal reconciliation and derived semantic boundary

A same-branch Regenerate admission is durable before its process-local `RequestRecord` is registered and queued. If the process dies in that window, SQLite retains `RegenerateAdmitted`, the original player `EventId`, and the character-specific `MessageHidden` / `CharacterKnowledgeForgotten` facts. It has **not** authorized provider dispatch. `DispatchUnknown` must never be interpreted as a replay permit.

## Terminal command ownership

`ChatApiState::inspect_orphaned_same_branch_regenerate(branch, request)` remains strictly read-only. `ChatApiState::abandon_orphaned_same_branch_regenerate(branch, request)` is a separate, explicit backend-only operation. The call chain is:

1. The application admission mutex serializes the same-process lookup against `RequestRecord` registration. A currently registered request returns `TimelineError::Conflict`; the existing live handle owns its lifecycle.
2. The Timeline ingress owner mutex isolates the process-local Timeline handle. Durable `RegenerateAdmitted` and its correct branch/request are independently validated by the existing receipt parser and replay-free recovery projection.
3. An existing `AcceptedReply` or `RegenerateAbandoned` is returned unchanged. Otherwise the owner attempts an append of `RegenerateAbandoned` on the original admission branch, including after `MayHaveDispatched`.
4. An optimistic SQLite `ConcurrentWrite` or domain conflict is reconciled read-only: **only** a verified durable `AcceptedReply` or `Abandoned` resolves successfully. Other storage failures stay errors.

The orphan command does **not** construct a new `RequestRecord`, reserve a queue permit, dispatch a model, infer provider success, apply state effects, fork a branch, or alter the original input. It can be invoked for an acknowledged orphan after restart; it is not called automatically by discovery or shutdown. A second process can have started an external provider call after its own durable dispatch marker; abandonment prevents subsequent Timeline acceptance but cannot retract an external call or already disclosed prompt.

## Executable acceptance envelope

B84 integration tests force the crash gap using a direct durable admission **without volatile registration**, then reconstruct `ChatApiState` and Timeline through SQLite. They verify read-only lookup before and after restart, no implicit abandonment on `close()`, explicit idempotent cancellation, refusal to operate on a locally registered request, unknown request refusal, preserved historical Mita knowledge, original player `EventId` and unrelated Kind-Mita event, archive contents, and no `RegenerateMayHaveDispatched` after pre-dispatch cancellation. A separate test verifies that foreign accepted reply/state effects are never relabeled as abandonment. Two independent SQLite owners attempt terminalization concurrently; exactly one durable abandonment exists.

## Semantic and summary boundaries

`SemanticMemoryRetriever` ranks **only** active `Fact` or `RagOnly` memories provided by a caller-owned Timeline snapshot. The embedding cache key includes character, embedding space, `MemoryId`, and exact content. Cached vectors cannot add a record absent from the supplied corpus, and content edits create a different embedding key. A B84 regression checks this and verifies manual `clear_cache()` physically clears the bounded cache.

These are three different claims:

- **Returned membership:** A fresh authorized Timeline projection cannot retrieve a forgotten record merely because an old vector is still cached; test-covered.
- **Resident memory:** A forgotten record's content and embedding can remain in cache until explicit `clear_cache()` or LRU eviction. No automatic event-derived purge or foreign-owner invalidation exists yet. `clear_cache()` does not guarantee removal of copies held by in-flight tasks, allocator pages, embeddings provider, or external systems.
- **In-flight provider:** A retrieval begun from an earlier snapshot can already have sent old content before Forget/Reveal/`CharacterContextReset`; the downstream acceptance fence does not retract it. The remaining gate needs an authoritative revision/freshness token or equivalent admission fence integrated at the provider boundary, not an undocumented global fallback.

`MemorySummary` is computed from each supplied `CharacterMemorySnapshot` in `MemoryListPage::from_snapshot`. There is no separate global mutable summary index to invalidate. A page captured before a foreign SQLite writer changes knowledge is intentionally stable and can be stale until explicit refresh; do not claim the page represents the latest world.

Public Regenerate remains disabled. Physical Unity effects, remote provider exactly-once, in-flight privacy, event-derived physical embedding purge, and automatic orphan enumeration/authorization remain outside the proved gate.
