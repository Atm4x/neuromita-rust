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

## Python SessionArchive parity boundary (verified 2026-11-07)

Read-only comparison against `Atm4x/NeuroMita` at
`5f3d061f523d9abefa6a264bb2953048a935fa65` (`bridge/session-timeline-reexport-20261103`),
using `src/domain/timeline/{archive,models,assets}.py` and
`tests/test_timeline_transfer.py`:

- Python envelope is `archive_version: 1` with flat `branches`, `events`,
  `operations`, `assets`. Rust's current native envelope uses
  `format/version`, branch-local facts/receipts and different binary fields.
  Equal version numbers **do not mean compatible formats**.
- Python `session_id`, `branch_id`, `checkpoint_id`, `event_id` and observer IDs
  are validated as arbitrary nonempty canonical strings. Rust session, branch
  and event IDs are UUIDs and observers are known `CharacterId`s. A lossless
  converter must retain **all original external identities** alongside any
  internal UUID mapping and reject collisions/unknown character mappings.
- Python `occurred_at` is timezone-aware ISO 8601 at **microsecond** precision;
  native Rust facts use Unix **milliseconds**. Exact re-export requires
  retaining the source timestamp, not a destructive conversion alone.
- Python images refer to `type: image_ref`, `asset_id: sha256:<64 hex>`,
  `media_type`, `byte_size`; Rust typed parts use `type: image` with an
  `asset: {digest,size,media_type}`. Both compute SHA-256 over MIME, a zero
  separator, and bytes, but the reference schema and accepted edge cases
  differ. Validate every binary before publishing any converted session.
- Python operations preserve an `intent` document and event ID lists;
  Rust receipts preserve typed drafts, command and fact results. A converter
  must prove equivalence of replay/idempotency, not merely copy JSON keys.

`tests/fixtures/python_session_archive_20261103.json` is a genuine canonical
Python SQLite export generated with the above source revision, including
Unicode session/message identities and a fork. The current Rust importer
**rejects** it without publishing a partial session. The fixture is a
regression input for a future explicit Python-to-Rust import boundary, not a
claimed implementation of Python parity. Do not add a schema migration chain
or synthesize lossy UUIDs. The existing Python app import/onboarding pipeline
is tracked separately by agent D; the archive identity contract belongs here.

## Explicit Python observer bindings (B24)

`InspectedPythonArchive::bind_memory` and `bind_sqlite` form a **read-only**
`BoundPythonArchive` against a destination snapshot. The caller must supply an
exact one-to-one mapping of every Python observer string to an existing Rust
`CharacterId`; no implicit names, defaults or aliases are inferred. Source
`PlayerMessage` actors must use the recognized Python `Player` spelling, and
character-owned events must have an explicitly bound author. Unicode external
IDs and original ISO-8601 timestamps remain on the inspected archive. Session,
branch and event candidate UUIDs must be unused across the destination.

Binding is **not** an import, identity reservation, or database transaction.
The destination may change after the snapshot. A future installer must
revalidate the mappings and collision set **inside the same destination write
transaction**, and must persist the original source identities and exact
microsecond timestamps atomically with canonical events. Neither the current
SQLite storage nor the in-memory store is mutated by this step.

## Pre-publication Python binary staging (B25)

After a successful source preflight and exact observer binding,
`BoundPythonArchive::stage_assets(&TimelineAssetStore)` copies only the
verified archive's binary closure into the already injected, canonical
content-addressed storage. External `sha256:<hex>` IDs are checked against the
returned native digest, size and media type, with idempotent import. No native
session, event, receipt, or identity mapping is committed by this method.
A subsequent failed transaction can leave orphan content-addressed blobs;
read-only orphan inventory is allowed, destructive GC is not. Callers must not
show these binaries as attached to a canonical event until the later Timeline
transaction succeeds. This API does not make Python SessionArchive directly
importable into native Memory or SQLite.

## Atomic session provenance persistence (B26)

Native SessionArchive now has an optional, strictly typed `provenance` field.
It is **metadata, not another event history**: it records external session,
branch and event keys, original RFC-3339 timestamps with Unix microseconds,
and explicit external observer-to-`CharacterId` bindings. Each mapped event
must exist in the canonical session and its microsecond timestamp must agree
with the fact's stored millisecond bucket. The root branch must be mapped.
Later native-only events and branches may remain unmapped; mapped external
identities are never fabricated for them.

Memory publication validates this metadata against the complete canonical
session before swapping the in-memory state. SQLite uses the same validation
inside `BEGIN IMMEDIATE` and writes `timeline_session_provenance` within the
same transaction as facts, receipts and branch rows. A failed provenance write
rolls back *all* of those rows; the SQL error-injection regression proves it.
Reopening SQLite reloads and validates provenance; native SessionArchive
export/import round-trips it without truncating Unicode IDs, offsets or
microseconds. The existing native archive format label stays unchanged; absent
provenance remains valid for native-only sessions. The new table is created by
normal pre-release database initialization, without a versioned migration
sequence.

`BoundPythonArchive::provenance()` produces this metadata detached from storage
using the earlier explicit binding. **It does not publish or convert Python
facts**, and staging Python assets remains a separate operation. The final
Python importer must still convert every event and receipt without altering
visibility or semantics (`EventObserved` is not yet representable), and pass
both canonical facts and provenance to the existing atomic installer. No
Python-to-Rust SessionArchive import is claimed by B26.

A second canonical session cannot reuse the same `(external source kind,
external session ID)` even when its native UUID differs. The check is made
when constructing the detached Memory candidate or inside the SQLite write
transaction and covers the metadata of existing sessions. This prevents
ambiguous re-import of one Python source under a different native identity;
it does not replace the final importer’s collision checks.

## Python canonical conversion preflight (B28)

`BoundPythonArchive::prepare_canonical()` constructs detached Rust `EventDraft`,
`TimelineFact` and operation `Receipt` values from the already validated Python
source, preserving the source IDs through the deterministic mapping and exact
microsecond timestamps in `SessionProvenance`. It applies explicit observer
bindings and rewrites causal references and fork checkpoint/parent IDs to native
UUIDs. It does not mutate Memory, SQLite or the AssetStore. The entire detached
result must satisfy native receipt replay and provenance validation.

This is a deliberately restricted compatibility boundary: Python
`EventObserved` disclosure, message presentation mutations and media references
remain **unsupported** until their semantics are translated without broadening
MitaView access or dropping content. The conversion errors rather than erasing
those events. No Python archive can be committed by B28; an atomic importer and
complete coverage of Python event kinds are still required for #9.
