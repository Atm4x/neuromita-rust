# Agent A Cycle58 — Verified known-files restore into a new data root

Cycle58 extends the offline recovery-snapshot operator flow from Cycle57. It restores **only** the known local persistent files in a verified, independently stored snapshot; it does **not** restore SessionArchive/History, installed AI models or external documents. The active application data root is never overwritten, replaced, selected, activated or modified.

## Operator interface

```text
neuromita recovery-snapshot create --root ABSOLUTE_DATA_DIRECTORY --output NEW_ABSOLUTE_DIRECTORY
neuromita recovery-snapshot verify --snapshot ABSOLUTE_DIRECTORY
neuromita recovery-snapshot restore --snapshot ABSOLUTE_DIRECTORY --output NEW_ABSOLUTE_DATA_DIRECTORY
```

The source snapshot must be a previously sealed `recovery-snapshot create` output, including a valid `recovery-manifest.json`, canonical `timeline.sqlite3`, and zero to four known optional configuration JSONs. It must be independently readable and be under operator control. The output must be an **absent path**, with an existing directory parent **outside the snapshot tree**. Stop all involved application processes and writers before operating; the program uses optimistic inspections, not an OS-wide exclusive lock. Never run this against an active data root.

## Implementation and invariant boundaries

1. `recovery_snapshot::verify` reads the strict versioned manifest and rejects unexpected/missing snapshot entries, invalid or unsafe JSON files, derived-index drift, invalid canonical data, SQLite integrity violations and mismatched Timeline fingerprint/revision/event count. The manifest is only a content-bound receipt, **not authenticated provenance**.
2. One fresh privately staged directory is created in the output parent (Unix 0700). `SqliteTimelineRepository::restore_verified_backup_into_new_database` creates a **new** coherent SQLite file using its canonical `VACUUM INTO` pipeline and the manifest's exact 64-hex Timeline fingerprint. This includes committed WAL data in the original snapshot if present. There is no base-SQLite-file copying, link sharing, SQL migration fallback or alternative Timeline store.
3. Present configuration JSONs are read via the bounded regular-file reader (no symlinks), written into newly created private files (Unix 0600), and each file is synced. Only the known literal filenames are accepted. Absent optional configs remain absent. **The snapshot manifest is deliberately not placed in the restored application root.** JSON values (possibly credentials) remain unencrypted and must be protected by the operator.
4. Fresh `recovery_inventory::inspect` of the staged output must exactly match the original snapshot manifest. After the private stage is complete, the input snapshot is verified independently **again** and the stage is re-inspected. These are optimistic checks, not cross-owner writer exclusion. Unknown external data owners remain excluded.
5. The existing shared directory publisher syncs the stage, then publishes the directory with Linux `renameat2(RENAME_NOREPLACE)` or Windows `MoveFileExW` without replacement. Unsupported Unix platforms fail closed. If a concurrent process creates the destination, the operation does not overwrite it. The privately owned stage is removed best-effort on prepublication errors, subject to Unix inode identity check; no already-published destination is deleted automatically.
6. A failure to sync the destination parent **after** rename returns `InventoryError::PublicationUncertain` without deleting the resulting path. The outcome may be visible but not proven crash-durable. Stop all writers, independently inspect the output if present, and use a different **new** destination for any retry. Windows parent-directory sync remains a no-op and crash durability is not asserted there.
7. The restored directory contains one independent SQLite Timeline and zero to four independently created JSON files. It can be inspected with `recovery-inventory inspect --root`, but it is **not activated** by this operation. No auto-cutover, no SessionArchive importer, no model/asset migration, no cross-owner consistency guarantees and no authentication of the snapshot or manifest.

## Verified tests

- Deterministic unit tests: separate output file ownership, destination-creation race (foreign file intact), late source mutation, postpublication sync uncertainty and restoration into the snapshot directory being rejected.
- Real separate-process CLI tests: restore after deletion of the original data root; exact canonical SQLite branch/event/receipt/selection/revision rows and configuration fidelity; restored JSON independence from the snapshot; malformed/changed manifest and wrong fingerprint; missing/modified config, alien entries, already-existing output file/directory, symlink destination and unsafe output locations. The recovery command works offline with `GEMINI_API_KEY` removed.
- Existing snapshot, inventory, Timeline backup/restore, SQLite integration and no-default-features regression suites are retained as validation gates. No GUI sources, builds or tests.

**Acceptance remains 16 PASS / 4 PARTIAL / 6 OPEN.** M7 and D5 stay OPEN until the separately owned SessionArchive/History and external assets have an explicit API and coherent recovery plan. The known-files subset alone does not satisfy complete user dataset restoration.
