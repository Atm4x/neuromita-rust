# Python preset import — existing Rust profile review (D6)

This extends the standalone conversion in `python-preset-staging.md`. It does **not** activate an import, edit a running Rust profile, or replace any original files. Python reference remains `Atm4x/NeuroMita@5f3d061f` (`Settings/api_presets.json` and `Settings/api_templates.json`).

## Offline review

Stop Python NeuroMita and the Rust application before taking the source snapshots. Run the existing example with the optional Rust data directory:

```text
cargo run --locked --offline --example import_python_presets -- \
  /old/Settings/api_presets.json /old/Settings/api_templates.json \
  /portable/UserData-import-reviewed --existing-dir /portable/UserData --dry-run

cargo run --locked --offline --example import_python_presets -- \
  /old/Settings/api_presets.json /old/Settings/api_templates.json \
  /portable/UserData-import-reviewed --existing-dir /portable/UserData
```

`--dry-run` reads and validates current Rust profile snapshots without writing. The non-dry-run command creates a **new directory** containing merged `presets.json` and `preset-credentials.json`, the Python source snapshots and their old Rust counterparts (`existing-source-presets.json`, `existing-source-preset-credentials.json`, when present), plus template descriptors. Current and Python data remain unchanged. If neither existing Rust catalog nor credential store exists, the explicit `--existing-dir` option fails rather than treating an unknown folder as an empty profile.

## Merge invariants

- Uses the current `StoredPresets` validation contract and original Python converter. Existing records and local credentials are not modified. Existing preset IDs are preserved. Imported IDs are also preserved, but any overlap with the existing or retired ID range `[0, next_id)` **blocks** conversion rather than silently renumbering or reusing IDs.
- Duplicate/invalid JSON, unresolvable local credential references, and duplicate incoming credential names block the whole review. A matching name blocks even when secret values appear identical. Secrets are never printed in preview or written into the catalog file.
- On Unix, staged directory and files are private. Existing raw source bytes are copied exactly and persisted in the staged directory for manual rollback review. Missing files in an explicitly selected existing directory are not confused with malformed files.
- Only the *new directory* is published after all components are prepared and synced, using the established staged-publication path. There is no two-file in-place replacement, partial activation, or automatic switching of `AppSettings` preset assignments.

## Explicitly unresolved

An active Rust profile uses two independent files and may have queued requests. Replacing them without a single transactional activation contract would risk partial installation, lost credentials or stale in-memory state. This stage deliberately refuses to implement such a swap. It also does not import Python custom display order, historical `custom`/list formats, templates as active presets, or Python's active selection and model fallback rules. Windows ACLs, power-loss behavior, rename races and live GUI recovery are not certified. Issue #45 remains open.
