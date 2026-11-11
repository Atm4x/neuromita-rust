# B85 — Timeline-owned semantic retrieval freshness and cache eviction

## Scope

Backend-only. `MessageHandler` sends the `TimelineTurn` minted by `CharacterHandle` from the canonical `SessionTimeline` to `KnowledgeRetrieval`. No independent semantic knowledge store and no UI/Presenter/Unity changes.

## Per-character barrier

`TimelineTurn` carries the selected branch/generation, captured world head and per-character knowledge epoch. The bounded embedding cache maintains only the last admitted turn and the set of active memory IDs for each character; it does not retain an additional unbounded plaintext snapshot. A scoped retrieval validates monotonicity of the selection generation and head, rejects an older/foreign snapshot, and prunes cached entries against **the caller's active Fact/RagOnly projection**. A branch/generation or Forget/Reveal/`CharacterContextReset` epoch transition evicts all of that character's cached embedding entries. Same-epoch changes, such as memory edits or expiration, prune entries whose content no longer belongs to the new active projection. Unrelated characters' cache entries are not evicted.

Every scoped retrieval increments a per-character in-flight revision before any provider await. This is conservative: a newer overlapping retrieval for the same character supersedes the older one even when the two snapshots happen to be identical. After a query/document embedding await and immediately before returning a result, old futures verify that revision and the global clear revision. A stale future returns `EmbeddingError::StaleKnowledge`; it cannot reinsert embeddings after eviction or complete a successful retrieval from an older observed snapshot. `clear_cache()` also advances the global revision rather than only emptying a HashMap. No lock is held across an embedding provider await.

The original standalone `retrieve`/`select` entry points continue to work with a caller-supplied snapshot, but they cannot bypass the freshness barrier after an authoritative turn has been registered for the same character. Production generation uses the new scoped entry point. Neither snapshot membership nor content is sourced from cached vectors.

## Storage and privacy limits

SQLite `context_epoch` and branch/head are queried from the repository when a `SessionTimeline` context is captured. Consequently a subsequent first-owner capture observes changes committed by another SQLite owner without relying on an in-process watch notification. Three real SQLite cross-owner tests cover Forget, Reveal and History Reset: next captured turn evicts cached embeddings, old turn is rejected, historical knowledge remains fixed at its original revision, and a different Mita's event stays visible. A separate test covers same-epoch memory edit and per-character eviction, and a deterministically blocked embedding provider demonstrates that an old in-flight result cannot repopulate the cache. No ordinary story is treated as a Reveal operation.

**This does not establish immediate multi-process physical purge.** If a different SQLite owner commits Forget/Reveal/History Reset while this process is idle, its cached vectors remain until it observes an authoritative new turn or another explicit invalidation. Embedding vectors and plaintext may also remain in already cloned heap buffers, provider runtime memory, swap, file logs or remote systems; removal from the bounded cache is **not** cryptographic zeroization. An external embedding provider can have received text before the event committed. The existing pre-provider dispatch and durable acceptance fences reject a stale request, but cannot revoke an outbound request already received by a model. Memory summaries and any future derived indexes require their own invalidation contract.

## Verification and remaining gate

B85 adds six regression tests for cross-owner Forget/Reveal/History Reset, in-flight reinsertion, explicit clear, and same-epoch edits / other-Mita isolation. The public Regenerate action stays disabled. To close the broader privacy gate: add top-level ownership for immediate local committed-event notification, a policy for external-owner observation while idle, an explicit purge boundary for any auxiliary summary/index caches, and tests covering late provider cancellation/disclosure. Do not claim an exactly-once remote provider contract.
