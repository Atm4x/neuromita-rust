# SessionArchive: selected canonical Timeline session

## Contract and identity

`storage::archive::{export_memory,import_memory,export_sqlite,import_sqlite}` transfers one `SessionId` using a single uncompressed JSON envelope: `format="neuromita.timeline.session"`, `version=1`, `session_id`, `root`, deterministic UTC `created_at_utc`, ordered `branches`, and binary `assets`. Unknown format/version and fields are rejected; there is no prerelease format migration chain.

Each archived branch contains its `BranchInfo` (`id`, `parent`, `checkpoint`, `fork_sequence`, head), **only its own local facts**, and operation receipts. Never export `world(branch)` as branch-local events: it includes the ancestor prefix and would duplicate event identities after import. Event UUIDs, causal refs, external **message** IDs, observer sets and original JSON payloads are retained. Parent/fork/checkpoint consistency and every receipt are checked using Timeline's existing domain validator. The archive is deterministic for a fixed store snapshot and identical assets; imported sessions re-export byte-for-byte identically.

Repeated import with an existing session ID is an explicit conflict, not an overwrite or ambiguous merge. Foreign event UUID collisions are rejected. An existing destination Timeline cannot be partially changed: the in-memory install is validated before swap, and SQLite uses one write transaction for all branches, facts and receipts. A SQLite failure midway rolls back the session. Original active selection is not silently changed.

## Asset consistency and access

Binary assets are included only when referenced by selected session facts, keyed by the same `TimelineAssetRef` digest/size/MIME used by `TimelineAssetStore`. Archive decoding validates base64, size and hash before Timeline publication; export refuses missing or corrupted files. Import files is content-addressed and idempotent. The importer deliberately imports verified blobs **before** committing Timeline facts: a later transaction failure may leave an **orphan binary**, but cannot publish a dangling Timeline reference. Archive-aware orphan garbage collection is deferred until live branches and retained archives can be accounted for.

Archive export is a **privileged full-session data operation** and intentionally includes hidden, reset and otherwise private facts when they belong to the selected canonical session. It is **not** a `MitaView` export. A caller must authorize export, target, and destination of all included content explicitly. No UI or provider path should invoke this operation on behalf of an individual character. It uses the injected AssetStore directory, not AppData/XDG or process cwd.

The current bounded in-memory representation limits the incoming JSON envelope to 96 MiB, each binary to 16 MiB, cumulative unencoded binary payload to 64 MiB, 256 branches, 256 distinct assets and 100,000 local facts. No new crates, compression stack or new portable-root policy are introduced.

## Unfinished acceptance criteria (#7/#9)

- **Python session identity parity remains open.** Rust UUID session IDs are retained exactly; Python string IDs need a durable explicit mapping/ownership contract persisted across reopen before Python `Histories` transfer. Preserving `external_message_id` does **not** solve the external session ID requirement.
- Real Python world/Histories fixtures and parity are not implemented/tested; this is a native Rust SessionArchive only.
- Archive publication is not yet wired into application onboarding/Slint. The importer rejects existing session IDs; a future explicit idempotent policy is possible but must not silently overwrite data.
- The binary-asset orphan retention lifecycle across all branches, saved archive files, and cleanup is not implemented. No unsafe GC is performed.
- SQLite transactionality covers canonical Timeline rows; AssetStore filesystem writes cannot join that database transaction. An unsuccessful commit leaves only unreferenced verified binary blobs, never a partially published session.

## Contract tests

`tests/timeline_archive.rs` covers memory and SQLite fork/receipts/causal/external-message-ID round trips, deterministic re-export, SQLite reopen, unknown or corrupt envelopes, binary integrity, cross-session ID collisions, failed import atomicity (including injected SQL error mid-write), and exact-scope export. Compile/run with existing `--locked --offline` dependencies. This does not claim Python fixture validation or production UI integration.
