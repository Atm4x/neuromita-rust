# B86 — Owner-controlled semantic cache reconciliation

## Ownership and authority

The Timeline remains the **only** source of character memory and knowledge. A
`KnowledgeRetrieval` instance is an application-owned derived cache, not a
memory repository. `ApplicationLifecycle` installs one bounded refresh worker
per character, owned by the existing `owners_stop` cancellation token and joined
with the existing owner tasks. There is no global event bus and no dependency
from the Timeline or Character domains to Knowledge.

Each refresh worker listens to the existing per-character Timeline memory and
context `watch` notifications and performs a **three-second idle poll**. These
notifications are local wake-up hints only; a different SQLite owner is not
required to publish them. Neither hints nor the previous cache snapshot are
accepted as fresh truth: a worker captures a new Timeline turn, checks whether
its version supersedes a previously tracked one, and then reads the currently
selected world under the shared Timeline mutex. The world is projected by
`character::projection::character_memories_at`, using the existing character
boundary. A second Timeline capture must still match before the cache adopts
the result; otherwise the attempt is deferred to the next wake-up.

When nothing is cached or tracked for a character, polling does not construct a
world projection. A changed `TimelineTurn` with an existing cache causes
`SemanticMemoryRetriever::refresh_authoritative` to use the very same
per-character epoch/revision fence as a normal retrieval, **without making
embedding-provider requests**. This evicts stale cache entries and invalidates
outstanding asynchronous work. Selection and knowledge-epoch changes purge all
character entries; memory edits prune only keys absent from the current active
fact/RAG projection. Other characters' entries remain intact.

## Concurrent retrieval correction

B85 advanced a per-character barrier for **every** scoped retrieval, even when
the version, admitted memory IDs and every cached key were identical. Two
concurrent replies for the same character and snapshot could therefore
spuriously reject one another. B86 preserves the barrier for identical observed
snapshots and still advances it when the authoritative snapshot or admitted
cache key set changes. The old in-flight task still fails with
`EmbeddingError::StaleKnowledge` after an actual superseding refresh.

## Proven by tests

Nine B86 regressions cover stable same-snapshot concurrency, cache-hit reuse,
barrier-enforced in-flight cancellation, unscoped cache entry pruning, a local
context-reset notification, foreign SQLite soft deletion, and foreign SQLite
Forget, Reveal and typed `CharacterContextReset` while this owner is idle. The
foreign cases use two independent SQLite connections; the owner refreshes
without a new generation request, and the original historical MitaView remains
immutable. Soft-deleted memory is absent from active retrieval but still
visible as `Deleted` in a newly derived `MemorySummary`; the summary is not a
separate persistent cache.

## Non-guarantees and costs

* A three-second polling interval is a **best-effort wake-up frequency**, not a
  strict maximum delay: scheduling, lock contention and SQLite failures may
  extend it. Without a new request there can be an interval during which a
  foreign writer has committed and another process still holds old embeddings.
* A foreign selection change that makes local Timeline reads fail currently
  defers refresh; no cryptographic or memory-safe fail-closed guarantee is
  asserted for that storage-error path. This is a follow-up gate.
* Polling can replay a world projection for each changed character. Workload
  and memory-growth benchmarks are required before selecting a production
  interval or consolidating polls by shared Timeline revision.
* Cache eviction removes map entries and fences in-flight re-insertion, but
  does **not** zeroize heap clones, deleted soft-memory records, or provider
  payloads. It cannot revoke a request already sent to a remote embedder or LLM.
* Current `MemorySummary` is derived from a newly requested
  `CharacterMemorySnapshot`; an already held page is intentionally historical
  and must not be represented as a live view. Derived summaries or indexes
  elsewhere require their own invalidation audit.
* No change enables public Regenerate, emits implicit Reveal, forks the
  selected branch, mutates past events, or changes the player's original
  `EventId`.
