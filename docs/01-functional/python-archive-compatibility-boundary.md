# Python SessionArchive compatibility boundary (Agent B22)

This implementation is based on `Atm4x/NeuroMita` at commit
`5f3d061f523d9abefa6a264bb2953048a935fa65` and the real SQLite export
fixture `tests/fixtures/python_session_archive_20261103.json`.

The canonical Rust Timeline remains the *only* history store. The new
`inspect_python_archive()` is a **read-only, detached preflight**. It neither
imports a session nor writes an asset or receipt. In particular, the ordinary
Rust `import_memory`/`import_sqlite` entrypoints still deliberately reject the
Python envelope. Never treat a successful preflight as a successful import.

## Preserved source identities and times

Python uses arbitrary nonempty string identities for sessions, branches and
events (plus independent external message and observer strings). Rust uses
UUID-based SessionId/BranchId/EventId and an enum-based CharacterId.

`PythonIdentityPlan` provides stable, session-scoped candidate UUIDs derived
from SHA-256 of a domain separator, identity type, source session ID and source
ID, with RFC 9562 UUID variant/version bits. It retains every source string,
checks collisions *within the archive* and offers `ensure_unoccupied` for
checking IDs held by a destination. These 128-bit candidates **are not a
bijective encoding of arbitrary strings**. The source-to-target map must be
persisted transactionally with the eventual imported session; until then the
mapping is a temporary plan, not an authoritative identity store.

Python timestamps retain their original ISO-8601 spelling (including offset)
and their exact Unix **microseconds**. Converting them only to Rust's
`occurred_at_unix_ms` would lose information. The ordinary Rust Timeline model
has not yet gained a durable, lossless external-time representation.

The Python fixture contains observer `A`, which is not a Rust `CharacterId`.
No guessing or fallback to `Crazy`, `Kind` or other built-ins is performed.
An explicit observer identity policy is required before import.

## Verified preflight scope

The parser limits archive and binary sizes; rejects unknown envelope fields or
archive versions, duplicate IDs, cycles, broken fork checkpoints, noncontiguous
branch sequences, invalid event types, missing receipts, mismatched operation
intents, causal links to unseen events, invalid message disclosures, and
inconsistent binary SHA-256/MIME/base64/length. Content-addressing is checked
against Python's `sha256(media_type + NUL + bytes)` convention. Validation
and exact-identity transport serialization have Rust fixture tests, with the
original Python `SessionArchive.from_document` parser serving as a read-only
reference for the fixture.

The preflight does not yet provide a transactional translation into either Rust
repository and is not a complete proof of Python→Rust behavioral parity.
A subsequent implementation must preserve source IDs/timestamps in the
canonical session's own durable metadata, map observers explicitly, convert
Python event/receipt variants without changing their meaning, and prove
atomic Memory/SQLite replay, conflict and export/re-import equality.
Do not create an auxiliary history database or silently coerce incompatible
fields. No GC authorization follows from this inspection.

A second fixture, `tests/fixtures/python_session_archive_asset_20261107.json`,
was re-exported and accepted by the **actual Python**
`SessionArchive.from_document` implementation with a real `TimelineAsset`
constructed by Python's `TimelineAsset.create`. Rust verifies the same fixture
and rejects a tampered asset. This verifies the cross-language hash/reference
contract, but still does not certify a native session import.
