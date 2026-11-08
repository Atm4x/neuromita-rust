# Cycle54 — SQLite Timeline snapshot publication uncertainty and independent inspection

## Correct post-publication failure contract

`SqliteTimelineRepository::create_verified_backup` and `restore_verified_backup_into_new_database` still stage a validated SQLite `VACUUM INTO` snapshot, sync the staged file and publish the new destination with an atomic, same-directory `hard_link` that refuses an existing path. However, `hard_link` success and subsequent directory `fsync` failure are **not equivalent to no publication**: the pathname may already be visible, and its survival after a crash has not been established.

Cycle54 introduces `TimelineError::SnapshotPublicationUncertain { destination, cause }`. This error is raised **only after successful hard-link creation when directory synchronization fails**. The implementation **does not unlink the destination** on that failure. Unlinking could otherwise delete an unrelated file after another actor replaces the directory entry. The scope guard still deletes the private, temporary staging pathname and sidecars; that is not a rollback of the published destination. Tests deterministically inject a failed directory sync both with an intact valid snapshot and after a foreign writer replaces the destination: the former remains inspectable, the latter is not deleted. A failed `hard_link` itself still returns a normal storage error and never overwrites another file.

If a caller receives `SnapshotPublicationUncertain`, it must **not** assume either a durable successful backup or an absent output. It must stop all other writers, independently inspect the current destination if it exists, compare the expected content fingerprint, and select a **new** path for any retry. A verified existing file is not proof that the directory entry survived a previous crash. With uncooperative concurrent writers, ownership of the destination pathname cannot be guaranteed after publication. Non-Unix `sync_directory` remains a no-op and cannot be advertised as a durable publication barrier; on Unix the guarantee is conditional on the filesystem honoring successful `fsync`.

## New read-only operator inspection

`neuromita timeline-backup inspect --database ABSOLUTE_EXISTING_SQLITE [--expect 64_HEX_FINGERPRINT]`

The independent `SqliteTimelineRepository::inspect_verified_snapshot(path)` operation reopens an existing SQLite Timeline in read-only mode, validates canonical+derived lookup parity, executes `PRAGMA integrity_check(1)` and foreign-key validation, then repeats the preview to reject observable concurrent changes. On Unix it also compares source inode/device identity. It reports revision, canonical event count and the same deterministic canonical/derived/DDL fingerprint as the original backup/restore path. With `--expect`, the CLI fails if the current fingerprint is not the operator-supplied token. It does not initialize a schema, write canonical events, repair indexes, delete files or change application configuration; SQLite's own WAL read infrastructure may still require sidecars.

The fingerprint **does not authenticate** the file or prove it originated from the user's installation. The caller must possess and trust the expected fingerprint independently; computing a token from the suspicious file and accepting it would only attest internal consistency. A complete cross-owner backup, SessionArchive recovery, cryptographic provenance, enforced process-exclusive maintenance and in-place active database cutover remain unimplemented.

## Actual gates

- 2 new deterministic native tests: failed directory fsync preserves the valid linked snapshot; a foreign file swapped into destination between link and injected failure is never deleted.
- 1 native inspection test: stable valid canonical source accepted and damaged derived-index schema rejected without repair.
- 2 new real process tests for `timeline-backup inspect`: a live-WAL source and its independent snapshot verify with a content-bound token, wrong token fails without mutation, damaged derived indexes and malformed SQLite fail closed.
- Existing backup and restore subprocess suites, Timeline SQLite persistence suite, full Rust library, CLI parser and no-default-feature binary check are separate regression gates.

**Formal M7/D5 remain OPEN**: this establishes a truthful failure contract and an operator verification path for **one SQLite Timeline file**, not full user-data-set recoverability or SessionArchive ownership. Overall acceptance is unchanged: 16 PASS / 4 PARTIAL / 6 OPEN.
