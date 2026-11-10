# Cycle52 — Verified restoration of a Timeline backup into a new SQLite file

## Ownership and limits

This command restores a **single canonical Timeline SQLite database** from an independently validated backup to a **new path only**. The Timeline repository remains the sole owner of canonical EventIds, branches, receipts, revision, active branch, and derived lookup tables. This is neither a complete application-dataset restore nor a SessionArchive import. It does not overwrite an active database, change application settings, start a server, or perform live cutover. External models, knowledge documents, and Agent B history files are out of scope.

## Operator workflow

1. Stop application writers and create a backup using `neuromita timeline-backup create --database ABSOLUTE_SOURCE --output NEW_ABSOLUTE_BACKUP`. Record its printed `source fingerprint` (64 hexadecimal characters). Store the backup securely; the fingerprint is **content identity and optimistic concurrency**, not authenticated provenance or a signature.
2. Restore **into a separate, nonexistent absolute path** in an existing directory:

   `neuromita timeline-restore into-new --backup ABSOLUTE_BACKUP --output NEW_ABSOLUTE_RESTORED --confirm BACKUP_FINGERPRINT`

3. Independently inspect and reopen the new SQLite file. Manually choose a safe future cutover only after all writers are stopped and all other application-owned files have been accounted for. **This command does not replace an active Timeline or change its configured path.**

## Safety contract

- An absent/non-SQLite/malformed input, a wrong or stale confirmation, broken canonical JSON, derived-index drift, SQLite structural or foreign-key integrity failure, an existing destination (including symlink), or a nonexistent destination directory causes **failure before publication**.
- The fingerprint is recomputed from canonical Timeline events, branch ancestry/head, receipts, active selection, logical revision, SQLite Timeline DDL, and derived lookup rows. The supplied fingerprint is required before creating a candidate and is verified again through the shared source validation and source/staging comparison.
- Restoration reuses the **same single verified SQLite snapshot primitive** as backup: native `VACUUM INTO` writes a new self-contained SQLite file through a temporary file in the destination directory, including committed WAL frames if the provided input happens to have them. The staged output passes SQLite integrity, FK, and canonical/derived fingerprint checks before publication. A final read of the input detects observable concurrent modifications.
- Unix staging file mode `0600`, file `sync_all`, atomic same-directory `hard_link` that **cannot replace an existing destination**, directory sync and temporary cleanup remain unchanged. Filesystems without hard-link support fail closed rather than copying over a destination.
- The confirmation is not a cryptographic proof that a backup came from this particular installation. An attacker able to replace both the file and the supplied fingerprint could supply any valid Timeline database. This is a trusted **offline** operator operation, not a remote authenticated endpoint, not a cross-process lock, and not a data-merging or salvage algorithm.

## Executable acceptance

`tests/timeline_restore_cli_process.rs` launches the actual `neuromita` CLI without `GEMINI_API_KEY`: it first produces a verified backup from a WAL database containing a context reset and multiple original event IDs, then restores it through the separate process. It checks the exact branch/event/receipt/selection/revision rows, derived preview and forgotten/known semantics after reopening. Further tests reject a wrong/malformed fingerprint, damaged backup, missing paths and existing destinations without modifying them. The restored database remains valid independently after deleting the backup source.

## Still open — M7 / D5

A safe cutover replacing an active SQLite file needs a distinct operator-owned, cross-process exclusive shutdown protocol and rollback strategy; it cannot be equated with writing a new file. Recovering corrupt/missing canonical events from scratch, restoring all external SessionArchive and app files, or authenticating a backup to a user remains outside this implementation. **M7 and D5 remain OPEN.**
