# Offline preset activation — reversible journal (D7)

This extends the existing reviewed Python preset staging and existing-profile merge. It is **not** a live migration, a versioned application storage schema, an updater or a general-purpose transaction manager. Source of truth remains `presets.json` plus the separate `preset-credentials.json`, accessed through their existing owner APIs. No Python source is modified.

## Historical Python source representations (D32)

Source reference: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`, `src/controllers/api_presets_controller.py`, especially `_load_presets_only`, `_migrate_presets_from_legacy`, and `_user_preset_from_dict`. These are formats accepted by the **active Python loader**, not revisions of the unreleased Rust storage schema.

`import_python_presets` accepts the same user-preset collection shapes from an operator-supplied Python source file:

- `{"presets": {"18": {...}}, "order": [18]}` — current keyed format;
- `{"presets": [{"id": 18, ...}], "order": [18]}` — historical list inside `api_presets.json`;
- `{"custom": {"any-key": {"id": 18, ...}}, "custom_order": [18]}` — historical `presets.json` or legacy-like `api_presets.json`, **user presets only**;
- `[{"id": 18, ...}]` — older top-level list.

All four are normalized **in memory** to the one existing Rust import plan; the original Python source bytes are retained in the staged private source backup. The historical `custom` object uses each record's own `id`, as the Python loader does, rather than treating arbitrary dictionary keys as preset IDs. The current keyed format follows the Python `id`-or-key behavior, but a present record ID contradicting its key is rejected instead of silently renumbered. Display order is retained if explicitly supplied, and a list without an explicit `order` retains its list sequence. A legacy `custom` map with multiple presets must provide `custom_order`: without it the JSON object's original insertion order cannot be safely reconstructed by this staging parser. References to unknown/duplicate user IDs are errors rather than silently discarded. The exact user selection in `LAST_API_PRESET_ID` is separately reviewed against converted IDs.

Unlike the Python runtime, the Rust import **never silently drops** malformed records, duplicate numeric IDs, unknown referenced order entries or unsupported preset fields. Staging fails or reports blocked IDs; no active profile is modified. A separate `api_templates.json` snapshot is still required so protocol mapping can be validated; the migration does **not** fabricate templates from the old file or a built-in default. A historical `presets.json` can be provided as the first CLI source argument even though the staged backup retains the stable filename `python-source-api-presets.json`.

The source snapshot contains no actual customer backups. This is source-format parity testing against redacted synthetic representatives, **not** certification that all historical customer profiles, API keys, custom model schemas and real GUI flows can be migrated.

## Strict offline workflow

1. Stop both Python NeuroMita and Rust NeuroMita, including Slint GUI and CLI. Wait for pending writes to finish. The command requires an explicit `--confirm-stopped` acknowledgement; it **does not detect or terminate** running processes. An external process writing either file during activation invalidates the safety assumptions.
2. Produce a reviewed directory using `import_python_presets ... --existing-dir /installed/UserData`, check its preview and any blocked IDs and verify the directory is new and separate from the installed `UserData`.
3. Activate using `cargo run --locked --offline --example activate_python_presets -- --apply /path/to/review /installed/UserData --confirm-stopped`. Both absolute directories must exist, be regular non-symlink directories and refer to distinct physical directories.
4. If interrupted, keep the application stopped. Run `cargo run --locked --offline --example activate_python_presets -- --recover /installed/UserData --confirm-stopped`. Do **not** manually delete `.preset-activation-journal` or copy individual files. Starting through `ApplicationConfig::persistent_catalog` refuses a pending journal and returns a recovery-required error.

Activation recalculates the reviewed output from the preserved Python inputs and the exact Rust baseline snapshots, and rejects any mismatch. It also validates the staged catalog, local credential references, JSON structure, bounded file sizes, and the fact that neither active source has changed since the review. Nothing is written to the active profile during this preflight.

## Transaction boundary

The application maintains a private `.preset-activation-journal` **inside the existing portable UserData**. The journal is temporary coordination data, not a new storage schema. Its old and new file bytes and explicit source-presence markers are written and synced before the durable `prepared` marker. Each destination file is then installed from a synced temporary file, with the replaced file moved into the private journal. Once both destinations are installed, the active directory is synced, then the `committed` marker is written and synced.

- Crash **before** `prepared`: no active files were modified; `recover` discards an incomplete journal.
- Crash **after** `prepared` but **before** `committed`: startup refuses to open the profile; explicit `recover` restores both original files byte-for-byte (including restoring original absence). A partially restored rollback can safely be re-run because old source snapshots are never consumed.
- Crash **after** `committed`: `recover` verifies both installed files against the committed snapshots and finalizes without rolling back successful data. A mismatch blocks cleanup for manual inspection.
- Journal cleanup first moves it to a private, uniquely named finished sibling. If a crash occurs during deletion, a finished directory may remain as sensitive cleanup residue, but a still-pending journal is never silently discarded during startup.

No secrets, JSON values, or credential contents are logged. On Unix, new journal files are `0600` and the journal directory is `0700`. Newly installed files are private. The D8 Linux regression suite also terminates a real subprocess at each activation checkpoint (`prepared`, first/second installation, and `committed`) and during the first recovery write. A fresh process verifies that startup refuses a pending journal, recovery preserves the correct old/new pair and a second recovery is a no-op. The termination hooks exist only in the library unit-test build, never the production binary. Recovery performs a two-file preflight: if an active file contains neither its recorded old nor new bytes, it refuses rollback and leaves the journal intact rather than destroying an external edit. On Windows, file-locking, ACL inheritance, file-replacement behavior, directory durability and application shutdown must be tested end-to-end before production certification.

## Important limitations

This is an **operator-invoked, stopped-app transaction**, not a live `PresetCatalog` reconfiguration. It does not provide cross-process locking against applications that do not participate in the journal protocol, nor a portable compare-and-swap transaction against concurrent external writers. An app kept open can persist stale in-memory definitions or credentials after activation; this is explicitly unsupported. The journal does not alter or delete Settings assignments. Power-loss guarantees depend on the host filesystem honoring `fsync` and rename; Windows directory sync is not certified. The result is a safe *recovery interface* for the supported offline workflow, not full issue #45 parity.

A subprocess crash test is not a power-loss or Windows locked-file test. Absent source files may be temporarily absent during interrupted installation, but unknown present data is never overwritten by recovery. A failed cleanup after the durable commit must be treated as a **committed-but-cleanup-uncertain** status, not rolled back blindly. If the original journal remains, explicit recovery verifies the committed values. If a `finished` sibling remains, do not remove it before determining that the active profile matches the reviewed output.

## Cooperative process exclusion (D9)

The persistent Rust application's composition root obtains an **exclusive OS file lock** on
`UserData/.preset-storage.lock` before opening its preset catalog and retains the
lease in the repository for the catalog's lifetime. Offline apply and recovery
also request a **non-blocking exclusive lock** on the same stable file and fail with
`StorageBusy` while any other Core or offline operation holds it. The file is never
unlinked, including after success, to avoid inode replacement and split-brain
locks. A symlink or non-file lock path is refused; on Unix the file is created
with `0600`. Pre-existing files are not silently truncated.

This excludes **cooperating Rust processes only**. The lock is advisory: an
external editor, older executable, Python application or direct filesystem
writer can still change the files. An explicit stopped-process confirmation and
snapshot verification remain mandatory; the lock is not a Windows ownership
certificate or a substitute for a two-file atomic commit. Recovery preserves
its journal when it detects foreign content. Kernel lock lifetime is scoped to
the acquired file descriptor (released by Drop/process death), rather than a
persistent PID file or stale timestamp. Existing unrelated file writers must
opt into this contract before they can be considered protected.

## D11 — optional three-document offline preference activation

A reviewed Python user selection can now be installed together with the preset
catalog and separate credentials **only** with an explicit operator action:

```text
cargo run --locked --offline --example activate_python_presets -- \
  --apply-with-preferences /absolute/review /absolute/UserData --confirm-stopped
```

This requires a valid, **existing** Rust `settings.json`. It neither creates a
new default settings profile nor imports unrelated Python settings. The reviewed
Python `LAST_API_PRESET_ID` must explicitly resolve to a converted user preset;
null, absent, template or unknown selections are refused. The review payload includes a SHA-256 fingerprint of the exact staged
Python settings bytes and is rederived from the original staged sources before any active write.
Older review packages without this fingerprint must be restaged with the current tool. Only
`AppSettings.presets.active` and the monotonically increasing `revision` change;
per-character assignments, speech, AI, locale and all other settings remain
unchanged. Custom Python display order is **not installed**, because Rust does
not yet have an owned display-order model.

The existing D7 journal remains the sole transaction owner. A private
`includes-settings` marker identifies D11 journals, which include old/new
snapshots and presence markers for all three files. D7 two-file journals remain
recoverable as-is. The `prepared` and `committed` boundaries apply to all three
files; recovery performs a full three-file foreign-write preflight *before*
changing any file and restores the exact old snapshots on an interrupted
uncommitted activation. Once committed, all three installed byte snapshots must
match before journal cleanup. Startup refuses a pending transaction.

D11 takes the existing `.preset-storage.lock` (to exclude cooperating Core
owners) and the settings repository's own `settings.json.lock` (to exclude
cooperating CLI settings writers) for the complete three-document apply or
recovery. Lock acquisition follows the preset-then-settings order. Neither
lock replaces the requirement that **both applications be stopped**. An
external Python app or editor which ignores these locks is not excluded, and
an all-or-nothing view for *live* external readers is not provided.

The original `--apply` action still installs **only two** files and leaves
`settings.json` unchanged, even for a review containing Python preferences.
Never manually remove or modify a pending activation journal. Linux process
abort tests cover `prepared`, after each of three installs, and `committed`;
Windows file-lock/ACL behavior and physical power-failure durability remain
unverified. This is not a three-file filesystem-level atomic rename.

## D28 — one-transaction import of supported Python settings and preset selection

The existing stopped-app, three-document journal now offers an **explicit** alternative to
selection-only activation. It combines the reviewed Python selected user preset with the
supported fields of the same byte-exact `python-source-settings.json` source:

```text
cargo run --locked --offline --example activate_python_presets -- \
  --apply-with-imported-settings /absolute/review /absolute/UserData --confirm-stopped
```

When no keys are unmapped, the command above remains available without a confirmation receipt.
For any unmapped keys, the combined operation fails closed. Its former boolean
`--allow-unmapped` is intentionally rejected: it cannot prove what was inspected.
Instead, with both applications stopped, run the review operation against the
**staged** Python source and the actual Rust target:

```text
cargo run --locked --offline --example activate_python_presets -- \
  --review-imported-settings /absolute/review /absolute/UserData --confirm-stopped
```

The read-only review displays the exact staged source SHA-256, exact existing
Rust `settings.json` SHA-256, names and counts of mapped keys and names and
counts of skipped keys (never values or credentials). The selected
`LAST_API_PRESET_ID` is separately validated against the staged user catalog
and is therefore excluded from skipped keys. Only if the operator accepts **each**
skipped name, use the displayed digests and one explicit `--accept-skipped-key`
per name:

```text
cargo run --locked --offline --example activate_python_presets -- \
  --apply-with-imported-settings /absolute/review /absolute/UserData \
  --confirm-stopped --expect-source-sha256 SOURCE_SHA256 \
  --expect-target-sha256 TARGET_SHA256 \
  --accept-skipped-key UNKNOWN_NAME_1 --accept-skipped-key UNKNOWN_NAME_2
```

The two digests and *exact sorted skipped-name list* must all match the new
preview, which is recomputed under the existing OS locks before the transaction
journal is created. The operation verifies them again inside the transaction
preflight, protecting against changes between CLI precheck and apply. A changed
known or unknown value, newly added unknown name, changed target profile,
invalid known value, stale staged presets or journal prevents activation.
Source JSON and Rust settings bytes are not logged. Neither preview nor
rejected activation writes an active settings file or creates a journal; the
stable OS lock files may be created as part of cooperative locking.

The combined activation receipt covers presets and three-file journal ownership.
The standalone `neuromita settings import --format python` now has its own
independent exact-source/target/skipped-key receipt, but uses a **single-file**
atomic replacement and cannot be mistaken for a three-file transaction. Its
former `--allow-unmapped` opt-in is rejected. Neither workflow is a complete
Python user-data backup; see `python-settings-import-consolidation.md`.

Supported settings are merged onto the existing, validated Rust `settings.json` (never generic
Python defaults), with the selected preset ID resolved against the staged preset catalog.
`presets.json`, `preset-credentials.json`, and `settings.json` are installed within the same
journal and same stopped-app lock lifetime. Existing `--apply`, `--apply-with-preferences`, and
`--recover` behavior is unchanged. Source files are read-only. The same journal supports
rollback before commit and verified cleanup after commit. This is still an offline workflow,
not live hot reload or a Windows power-loss certification. Full Python format coverage and
production GUI migration remain open in #45.

## D33 — fail-closed preflight for semantics the Rust preset cannot represent

The original Python `ModelSettingsService.resolve` reads optional per-preset
`schema_override`, `sparse_response`, and capability switches such as
`support_overrides.tools_native` / `streaming`. The current Rust preset
converter does not implement their full model-schema and capability semantics.
A successful conversion must therefore **not silently discard** those values.
Before creating any staged files, the D-owned staging boundary blocks a preset
containing nonempty custom schema overrides, an explicitly supplied sparse
response policy, unsupported capability flags, or duplicate enabled model-setting
IDs. It also blocks meaningful extra routing/test URL, preferred-model,
reserve-key distribution, model-profile or known-model choices that are not
represented in the current Rust preset definition. Empty/default optional
values and the supported `structured_output` switch remain accepted.

Older Python presets with missing/null `model_settings` remain blocked: Python
may regenerate them using the protocol registry, model profiles, legacy
`Settings/settings.json` and an `APIModelSchemas` override directory. None of
those data sources can be reconstructed safely from `api_presets.json` and
`api_templates.json` alone. No synthetic defaults are substituted; import
remains selective, and the original Python source is always a separate
read-only input (with a private byte-exact stage backup only after successful
staging). A custom external `APIModelSchemas/*.json` directory is **outside
this import contract**, not silently imported. Its compatibility must be
reviewed separately before claiming full backup/restore parity.

The Python `UPDATE_CONTOUR` setting is likewise an explicitly skipped
application-updater preference until the Rust updater owns an equivalent
setting. Do not reinterpret it as a Python Core updater instruction.

## D34 — native Windows subprocess interruption acceptance

The test-only crash injection now runs on both Unix and Windows. Each child
writes an exact checkpoint acknowledgement immediately before `abort()`, so a
failed process launch or ordinary test panic cannot be mistaken for an
interrupted journal transaction. The parent validates that acknowledgement,
observes the stopped child, then checks recovery and idempotence after every
two-document and three-document installation boundary, including a second
abort during rollback. Unix additionally requires the actual `SIGABRT` signal.

A Windows-only test holds an exclusive operating-system handle to the existing
`settings.json` and verifies that offline three-document activation refuses to
write any target or create a transaction journal. After that handle closes,
activation can complete against the unchanged original settings bytes.

These tests exercise real process termination and Windows sharing rules, but
do not certify system power-loss durability, arbitrary foreign writers,
Windows ACL inheritance, or user-facing GUI confirmation. The offline
stopped-application requirement remains mandatory.
