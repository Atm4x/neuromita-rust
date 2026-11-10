# Agent A Cycle57 — Explicit known-files snapshot

This is an **offline, deliberately incomplete** recoverability operation layered on the read-only `recovery-inventory` introduced in PR #357. The authoritative SQLite Timeline owner remains `SqliteTimelineRepository`; the four configuration JSON files are read from their exact known names. No new database, SessionArchive importer, or background scheduler is created.

## Operator interface

```text
neuromita recovery-snapshot create --root ABSOLUTE_DATA_DIRECTORY --output NEW_ABSOLUTE_DIRECTORY
neuromita recovery-snapshot verify --snapshot ABSOLUTE_DIRECTORY
```

The source root must contain an actual `timeline.sqlite3`, optionally `settings.json`, `presets.json`, `preset-credentials.json`, `finetune.json`. The source and output parent are existing directories; output must be outside the source tree and **must not already exist**. Stop the application and all external writers first. Creation emits only a path, Timeline fingerprint and `complete dataset recovery: false`, not credential material. The snapshot includes a standalone `timeline.sqlite3`, the exact-present JSON files and `recovery-manifest.json`. Validation is independently repeatable, including after removal of the original source. The inventory's explicit `unverified_owners` list is retained.

## Safety and correctness

1. Inspect the original source read-only, validate SQLite canonical/derived/DDL fingerprint and bounded JSON inputs. Staging uses a fresh sibling directory; on Unix it is created mode `0700`, and staged JSON+manifest files mode `0600`.
2. Create the SQLite file via the **existing** `SqliteTimelineRepository::create_verified_backup` (`VACUUM INTO`, includes committed WAL frames) instead of copying the base SQLite file. Explicitly compare the SQLite receipt with the original inventory.
3. Read each present configuration file through the existing bounded, inode-aware input reader, copy to a fresh stage filename and fsync. Independently inspect the stage and require the **same inventory** as the original source.
4. Write the manifest once, fsync it, verify the staged snapshot, require an exact filename set, then independently inspect original and staged data again. These pre/post comparisons are **optimistic only**: a writer can change and revert content between checks. They do not implement application-wide writer exclusion or a cross-owner transaction.
5. Fsync the stage directory where supported. Publish the entire stage directory under the new output name with **atomic no-replace rename** (`renameat2(RENAME_NOREPLACE)` on Linux; `MoveFileExW` without replace on Windows). Other Unix platforms fail closed rather than silently use overwriting `rename`. A concurrent destination-creation race fails without overwriting the competitor's content. On failure before publication, privately owned staging files are removed best-effort. A staging directory whose inode has changed is not recursively removed.
6. Fsync the output parent on Unix. If this final barrier fails **after** the no-replace rename, the API returns `InventoryError::PublicationUncertain` and leaves the output untouched: its crash durability is uncertain. Windows does not currently provide a directory fsync barrier, so the published directory is not claimed crash-durable there.
7. `verify` requires strict versioned inventory parity for the sealed directory and exact expected filenames. It refuses extra files, symlinks, missing files, modified configuration bytes, malformed JSON, corrupted Timeline and modified manifest scope. The manifest is **not signed**, and an attacker who can rewrite both data and manifest can forge internal consistency; use trusted out-of-band provenance to establish origin.

## Boundaries and acceptance

This snapshot covers **only one SQLite Timeline and four known JSON files**; it may contain sensitive credential values and must be stored in operator-controlled secure storage. It is not encrypted. There is **no automatic restore**, no active-data replacement, no process-exclusion lock, and no SessionArchive, game documents, external models/plugins/voice assets, or cross-owner consistency. `complete_dataset_recovery` is intentionally `false`. Restore into a NEW SQLite path remains the separate Timeline operation from PR #349; no cross-owner cutover is attempted here.

Cycle57 adds independent real CLI subprocess tests for standalone snapshot, damage, no-clobber and verification, plus deterministic unit tests for concurrent destination creation, changing source and postpublication directory sync failures. Existing library, binary, Timeline and backup/restore suites remain regression gates.

**Formal acceptance remains 16 PASS / 4 PARTIAL / 6 OPEN** (M7/D5 OPEN; D4/R5 PARTIAL; R6/R7 OPEN). Completion requires real SessionArchive owner contracts and a separately designed coherent/full-data restore protocol.
