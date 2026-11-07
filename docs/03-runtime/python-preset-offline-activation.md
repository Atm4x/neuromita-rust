# Offline preset activation — reversible journal (D7)

This extends the existing reviewed Python preset staging and existing-profile merge. It is **not** a live migration, a versioned application storage schema, an updater or a general-purpose transaction manager. Source of truth remains `presets.json` plus the separate `preset-credentials.json`, accessed through their existing owner APIs. No Python source is modified.

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
null, absent, template or unknown selections are refused. The review payload
is rederived from the byte-exact Python sources before any active write. Only
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
