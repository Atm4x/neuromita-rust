# ABCD Iteration 8 — SQLite-authoritative Knowledge provider dispatch

Base: `a6612788dcb4a087e0f98957fa66c98585ae22f8`. Target: PR #416, **Draft**, never `main` directly.

## Why the previous check was insufficient

The per-request `TimelineTurn` was compared to a live Core Timeline at selected stages, and the local derived embedding cache carried a revision. In WAL mode a separate SQLite writer can commit between any completed read and the next external embedding/rerank invocation. In particular, canonical memory upserts/deletes and hidden-history changes are not equivalent to the local cache revision. A result-only postcheck cannot prevent sending stale source text to an external provider.

Without a common protocol respected by **every writer**, or a database-level write reservation covering the outgoing call, one cannot claim a zero-gap property in the presence of arbitrary independent SQLite writers. An optimistic read on its own is not an atomic dispatch barrier.

## Implemented boundary

1. `KnowledgeDispatchSource` describes the exact canonical `MemoryId` or history `EventId`, and the text admitted for external use. It is an input to the Timeline owner; it is not stored in the derived embedding cache as a second authority.
2. `SemanticMemoryRetriever` passes these sources with its pinned `TimelineTurn` before query embedding, before each document embedding batch, and before reranking. A standalone API without an owned Timeline turn retains its existing caller-scoped contract, not the persistent live-generation guarantee.
3. The Core-owned `SessionTimeline` delegates authorization to its `SqliteTimelineRepository`. The repository opens a dedicated **existing** database connection and obtains `BEGIN IMMEDIATE`, then checks the persisted selection and generation, context epoch, knowledge of the pinned trigger, bounded branch head, and canonical live memory/history projection **inside that same reservation**. A missing, forgotten, deleted, updated or concealed source rejects the whole invocation before the provider receives it.
4. An opaque RAII `KnowledgeDispatchLease` holds the SQLite write reservation across the provider `await` and releases it through `ROLLBACK` on normal completion, error, unwind or cancellation. While it is held, independent SQLite connections cannot commit any Timeline write, including connections outside the project's repository implementation. The `SessionTimeline` mutex and the embedding cache mutex are **not** held during the provider await.
5. After the external call returns, Core rechecks Timeline authorization and the cache-generation barrier before emitting results. A changed source does not silently fall back to lexical or stale vectors. Same-process and independently reopened SQLite writers use the same database reservation, not an in-memory per-process mutex.

## Deliberate tradeoff and boundaries

This is a correctness-first **short-term admission protocol**. A transaction remains active during the remote call because the provider transport currently cannot expose a trustworthy, atomic point at which outbound bytes are irrevocably sent. This can delay SQLite writers for the duration of an embedding/reranking call; the provider's own deadline and cancellation behavior consequently matter. `BEGIN IMMEDIATE` does not stop bypasses such as replacing the entire database file, changing a copied database, or running a provider without a live admission turn; these are different authority boundaries. Do not claim general rollback of model-observed content or security against arbitrary modification of the underlying filesystem.

This guard does **not** cover external Documents corpus files, calls outside this scoped semantic provider path, historical caller-scoped APIs with no live `TimelineTurn`, or native Embedding/Reranker worker protocol implementation. Those remain separate acceptance gates. The desired future optimization is a versioned dispatch/commit protocol with independently enforceable writer coordination, so network calls need not hold a SQLite reservation; until all writers respect such a protocol, dropping the reservation would reintroduce the race.

## Verification

Run with Rust 1.98.1, default SQLite feature:

- `cargo check --tests --offline -j1`
- `cargo test --lib --offline -j1`
- `cargo test --test rag_history_generation --offline -j1`
- `cargo fmt --all -- --check`

New regression coverage: independent writer blocked while the lease is held, successful continuation after cancellation/release, stale selection rejection, forged source text rejection, and source deletion from a foreign SQLite connection. No actual external LLM/GPU and no Windows GUI/Unity integration were executed in this iteration.
