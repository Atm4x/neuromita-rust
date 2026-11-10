# Retained MemoryRecord transfer — versions 1/2 (Agent A, Cycles 26–27)

**Scope:** this is a *character-scoped transfer of retained `MemoryRecord` values*, not a
backup of `world.db`, the `SessionTimeline`, the conversation or game state. The
active Python `world-db-export` version 1 (`history`, `memories`, `variables`) is
**a different format** and is not accepted by this API. This deliberately limited
operation does **not** satisfy the full D4 acceptance gate.

## User workflow (trusted local CLI)

1. `/knowledge memory export /path/to/backup.json` exports retained memory records
   for the selected character. Version 2 is written to a **private 0600 staging
   file** on the destination filesystem, synced and only then published with a
   no-clobber hard link. The destination is never an incomplete file and never
   overwrites an existing path, even under concurrent exporters. The staging
   link is removed and the directory synced before success is reported.
   If the filesystem refuses hard links, the command fails rather than silently
   using a non-atomic copy. A failure after publication is reported distinctly
   as **published, durability/cleanup unconfirmed**, so the caller does not
   mistake a retry failure for absence of data. A process crash can leave an
   unlinked private `.neuromita-memory-*.pending` staging file; inspect before
   manual cleanup. The exported file records the original character.
2. Select the target character explicitly using `/character Kind` or another
   catalog ID. `/knowledge memory preview /path/to/backup.json` validates the
   bounded JSON archive and the **current target Timeline selection**. The preview
   displays source and target, total records, newly inserted and identical records,
   and a one-use confirmation token. Preview does not modify canonical data.
3. `/knowledge memory apply TOKEN` consumes the token and submits **one atomic
   character-owned batch**. Wrong tokens, other-character previews, stale snapshots,
   invalid records and cancelled requests cannot write partial imports. Repeated
   import of identical records is a no-op with no new Timeline memory changes.

## Format and validation

The versioned JSON envelope has exact top-level fields: `format`, `version`,
`scope`, `source_character`, `records`, and (since v2) `checksum_sha256`:

```json
{
  "format": "neuromita-retained-memory-records",
  "version": 2,
  "scope": "retained_memory_records",
  "source_character": "Crazy",
  "records": [],
  "checksum_sha256": "<64 lowercase hexadecimal characters>"
}
```

`checksum_sha256` is SHA-256 of a deterministic JSON serialization of the
payload tuple `(format, version, scope, source_character, records)` excluding the
checksum field. A v2 archive with a changed valid memory field or source character
is rejected *before* import preview. The checksum detects accidental corruption;
it is **not** a digital signature or proof of trusted origin, because a malicious
writer can recompute it. Older v1 archives are accepted only without a checksum,
so the previous exported files remain importable; their integrity is not
cryptographically checked. Unknown keys, invalid record fields and duplicate IDs
continue to fail strict validation.

`records` are the canonical typed `MemoryRecord` DTOs. All supported fields
(including UUID, content, kind, priority, entities, protected, Active/Forgotten/
Deleted state and expiration) are preserved. `source_character` describes the
origin; the target is always supplied explicitly to the import API, never
inferred from an untrusted archive. Unrecognized fields, unsupported versions,
malformed JSON, duplicate UUIDs, invalid records or a conflict with any existing
nonidentical ID are rejected. Normalized-content/island uniqueness and active
capacity are enforced by **the same** `CharacterMemories::resolve` and guarded
Timeline append as manual edits. No replace/force-overwrite conflict policy.

One transfer is limited to **256 retained records and 1 MiB JSON**, to fit in a
single atomic Timeline memory batch. Exports above that limit fail explicitly,
not with a silently truncated archive; they are not usable backups. A protected
or deleted record is restored as the exact record value, without changing user
protection, reactivating deleted entries or extending TTL. A target with conflicting
content or IDs fails before confirmation. No new mutable registry or database.

## Excluded categories and remaining acceptance

- **Not included:** Timeline conversations/events, game/world state, settings,
  character working state, variables, semantic embeddings, manual or extracted
  graph relations, source revision numbers, receipt IDs, media assets or files.
  Derived embeddings and graph relations can be recomputed separately; manually
  authored graph relations are **not** covered by this memory-only archive.
- No all-character export, date/status filtering, huge streaming archives,
  persisted staging plan, *authenticated signature*, Python `world-db-export` loader or
  full session continuity. The origin label is not cryptographic evidence.
- No import over another character without a deliberate target selection, no
  remote route, no automatic conflict resolution, and no rollback of external
  effects (there are none in this operation).
- D4 remains **OPEN** until user-required `history` and `variables` transfers,
  complete scope and mappings, and large-state handling are integrated through
  their canonical owners (SessionTimeline belongs to Agent B). D5 also remains
  **OPEN** pending system-level recovery and partial-failure workflows.

## Verified scenarios

`tests/memory_transfer.rs` exercises exact multi-kind multi-state transfer,
protection/TTL metadata, cross-character isolation, replay/no-op, malformed
version/unknown keys/duplicate UUID/oversize, incompatible ID, cancellation,
missing confirmation, stale edit and SQLite reopen with a real running
`ApplicationLifecycle`. Cycle27 adds v2 checksum mutation/legacy v1 fixtures,
private staging, fail-before-publish cleanup, target symlink protection, file
no-clobber and concurrent exporter races. The local CLI tests cover malformed
commands, one-use confirmation and non-overwriting durable file publication.
This is not full Python parity.
