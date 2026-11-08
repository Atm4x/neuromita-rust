# Cycle51 — Verified offline backup of canonical SQLite Timeline

This is a **new backend maintenance operation** for a single, existing Timeline SQLite database. It does **not** reconstruct corrupt/missing canonical events, repair SessionArchive, or transfer any external application files. SQLite Timeline remains the sole authoritative owner of its events, branch lineage, receipts, selected branch and revision.

## Operator command

`neuromita timeline-backup create --database ABSOLUTE_SOURCE.sqlite3 --output NEW_ABSOLUTE_BACKUP.sqlite3`

Run offline with the application and other writers stopped. The source must exist. The output directory must exist and the destination must **not** exist (including symlinks). Neither GEMINI_API_KEY nor a GUI is required. No implicit restoration or fallback is performed.

## Source validation and exact backup contract

1. `SqliteTimelineRepository::inspect_derived_indexes` reads the original source, loads the authoritative persisted Timeline and verifies that the four derived lookup tables agree with canonical events; inconsistent/missing indexes cause a **refusal**, not an invented repair. The existing 100,000 event / 500,000 rows per derived table / 10,000 schema object preview limits still apply.
2. An SQLite **read-only** source connection executes `PRAGMA integrity_check(1)` and `pragma_foreign_key_check LIMIT 1`. Any structural or FK failure refuses backup. No startup schema migration is run.
3. SQLite `VACUUM main INTO ?1` creates a temporary standalone database in the **destination directory**. It captures a consistent committed SQLite snapshot, including committed WAL frames even if they have not been checkpointed to the main `.sqlite3` file. It is not a raw `copy(source.sqlite3, backup.sqlite3)`.
4. On Unix, the temporary file is restricted to mode `0600` **before publication**. Validate the temporary standalone DB with SQLite integrity and foreign-key checks, then rerun the canonical/derived preview. Its fingerprint must match the source's original canonical events, branches, receipts, selected branch, logical revision, schema and derived rows exactly. A final source preview must still match the first preview. A concurrent source change that the pre/post comparison detects causes a typed refusal, with no output publication.
5. `sync_all` the staged file, then atomically publish it with `hard_link(stage, destination)` in the destination directory, which **never overwrites** an existing name. On Unix, also sync the destination directory. The temporary filename is removed on normal completion or failure. A destination filesystem that does not support hard links must fail instead of using a non-atomic replacement fallback.

The output is a self-contained SQLite file, not a base database that depends on the source `.wal` or `.shm` sidecars. It retains canonical EventIds, receipts and reset semantics. The receipt returned by `create_verified_backup()` reports revision, event count and the verified fingerprint; it does not claim cryptographic authentication or durable backup of other files.

## Executable proof

`tests/timeline_backup_cli_process.rs` spawns the **real** `neuromita timeline-backup create` process without GEMINI_API_KEY. A separate source SQLite connection remains open with WAL autocheckpoint disabled; the tests assert the WAL is nonempty before backup and that the independent reopened backup contains exactly the source canonical rows and the original EventIds, selected branch, operation receipts and pre-/post-reset knowledge. A corresponding repository preview must match exactly. Other fixtures verify no overwrite of an existing destination/source, no output on invalid source, missing paths, and derived-index drift. On Unix the backup must be `0600`.

## Unclosed M7/D5 scope

No restore command, complete cross-file user dataset manifest, SessionArchive ownership, multi-process exclusion, online active-writer quiescence handshake, or corrupted canonical event/page recovery. A SQLite committed snapshot is consistent, but the optimistic pre/post fingerprint is **not** a distributed process lock; callers must stop other writers. Successful tests for one Timeline file do not close formal M7/D5, which remain OPEN.
