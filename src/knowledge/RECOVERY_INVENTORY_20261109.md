# Cycle55 — read-only recovery inventory (known local files only)

## Verified owner boundaries

`ApplicationConfig::persistent_catalog` configures canonical `timeline.sqlite3` and four conventional JSON paths: `settings.json`, `presets.json`, `preset-credentials.json`, `finetune.json`. The SQLite Timeline is the authoritative persisted store of the canonical events, branch facts, receipts, character memory and variables in this branch. Existing `SqliteTimelineRepository::inspect_verified_snapshot` provides canonical/derived/DDL fingerprint, SQLite integrity and foreign-key verification (also considering committed WAL data). The JSON files remain owned by their existing settings/presets/finetune modules; inventory does **not** parse their domain schemas, expose secrets, import them or duplicate their owner logic.

The current Agent A Git tree has **no SessionArchive owner implementation**. The Agent B SessionArchive/History importer cannot be simulated or combined with independent `CharacterHandle::import_portable` calls without an agreed atomic boundary. AI plugins/models/voice, external documents, game files and settings stored outside these conventional paths are also not included.

## Added operator preflight

```text
neuromita recovery-inventory inspect --root ABSOLUTE_DATA_DIRECTORY
neuromita recovery-inventory verify --root ABSOLUTE_DATA_DIRECTORY --manifest ABSOLUTE_JSON_PATH
```

The trusted offline command never writes to the source, creates a backup, switches active storage, overwrites an output or initiates an import. `inspect` prints strict versioned machine-readable JSON (`neuromita-recovery-inventory`, v1, scope `known_persistent_local_files_only`). It reports canonical Timeline fingerprint/revision/event count, bounded SHA-256 hashes and sizes for present optional configuration files, and explicitly lists absent optional files. It prints no JSON payload or stored credentials. Missing or malformed SQLite, derived drift, corrupt JSON, oversized JSON (32 MiB per file), file symlink substitution, missing files and detectable concurrent changes fail closed. Timeline and configuration scans are repeated for optimistic stability. The manifest contains only root-relative known filenames and is portable to an independent root containing equivalent data; no file copying is performed.

`verify` accepts only an independently retained, strict v1 manifest up to 64 KiB and reruns all inspectors; it rejects any changed contents/known-path presence, unsupported scope, unknown keys and any manifest claiming complete recovery. The content hashes detect accidental changes but are **not authenticated** and cannot establish provenance. As there is no cross-owner snapshot lock, neither pass guarantees atomic capture of all data under concurrent application writes. On some operating systems WAL sidecar access can cause SQLite system bookkeeping; application-level canonical writes are never requested. For reliable operator use, stop the application and other writers.

The fields `complete_dataset_recovery: false` and `unverified_owners` are deliberately mandatory. They cannot be promoted to complete without SessionArchive-owned export/preflight/import, external-file owner contracts, staged/coordinated durability, and crash/restart and partial-failure acceptance. Current D4 remains PARTIAL; M7 and D5 remain OPEN. The recovery inventory is a **diagnostic prerequisite**, not a restorable dataset artifact.

## Tests

Separate-process `tests/recovery_inventory_cli_process.rs` covers valid Timeline plus settings secrets (must not appear in stdout), manifest re-verification, changed bytes and file additions, forged completeness/unknown JSON fields/fingerprint, corrupt JSON and SQLite, absent source/relative paths, and Unix symlinked config. Run without `GEMINI_API_KEY`, GUI, HTTP or provider initialization. No Agent B or Legacy/Obsolete files changed.
