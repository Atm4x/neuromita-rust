# Cycle53 — SQLite Timeline snapshot failure boundaries

The backup and into-new restore commands share `SqliteTimelineRepository::publish_verified_snapshot`. Both are trusted, offline operations for an existing valid Timeline SQLite database. No automatic application data cutover, no overwrite, and no SessionArchive recovery is performed.

## Verified failure properties

- The source path is now checked for Unix filesystem object identity (`st_dev`, `st_ino`) at both the start and end of snapshot preparation. Its canonical/derived/DDL fingerprint is also rechecked. Replacing the source pathname with a distinct inode containing **identical bytes** during the snapshot is rejected before publication; a matching fingerprint alone did not detect this case previously. This is optimistic path stability detection, **not** a live-process lock or a guarantee against an adversarial remove-and-reinsert (ABA) race.
- The already verified staging SQLite file is owned by a scope guard. If a late prepublication step returns an error, the staged main file and SQLite sidecars are cleaned. Sidecar paths are now constructed through `OsString`, preserving the original raw filename bytes, including Unix non-UTF-8 paths.
- Final publication remains `hard_link(stage, destination)` in the same directory. When an uncooperative process creates the destination **after initial destination preflight but before publication**, hard-link creation fails rather than overwriting the newly present destination. The staged file is deleted on error. This does not authorize an overwrite or in-place repair.
- Four deterministic native Rust unit tests verify (1) explicit failure after verified staging with cleanup and original data unchanged, (2) destination file creation at the publication boundary with byte-for-byte no-clobber, (3) Unix source inode replacement preserving the exact fingerprint still rejected, and (4) Unix sidecar removal with non-UTF-8 filename bytes. Real subprocess backup/restore regressions remain independent acceptance gates.

## Deliberate limitations

On non-Unix platforms, the source check remains fingerprint-based because `st_dev`/`st_ino` semantics are not portable. Directory fsync errors after linking the final output remain a distinct durability-uncertainty window; an uncooperative process could also mutate the source after the final check. A correct process-exclusive maintenance protocol would require cooperation of all writers/readers and is not implemented here. No assertion of unconditional crash-safe live cutover is made.

These harden the existing standalone Timeline DB backup/restore path, but do not close M7 or D5, which require a user-data-set-level recovery and SessionArchive ownership contract. Other acceptance slots are unchanged.
