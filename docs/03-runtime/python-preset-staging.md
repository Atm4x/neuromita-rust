# Python API preset staging — Agent D (2026-11-07)

**Scope:** safe, *standalone* conversion of the current Python `Settings/api_presets.json` and `Settings/api_templates.json` into a **new**, empty output location. Not an in-place migration of an installed NeuroMita profile; issue #45 remains open.

Reference Python source: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`, `src/controllers/api_presets_controller.py`, which persists `{"presets": {"id": ...}, "order": [...]}` and `{"templates": {"id": ...}}`. Rust already owns a validated `PythonPresetImport` mapper under `generation`; this change builds a non-destructive publication boundary around that existing converter, without changing provider settings semantics.

## Invocation

```text
cargo run --locked --offline --example import_python_presets -- \
  /path/to/Python/Settings/api_presets.json \
  /path/to/Python/Settings/api_templates.json \
  /absolute/portable/UserData-import-review --dry-run

cargo run --locked --offline --example import_python_presets -- \
  /path/to/Python/Settings/api_presets.json \
  /path/to/Python/Settings/api_templates.json \
  /absolute/portable/UserData-import-review
```

The example prints only numeric converted/blocked IDs and counts. No API keys, template private fields or blocker values are logged. `--dry-run` performs validation and preview without creating any files. The target must **not already exist**. The intended workflow is to stop both applications and review generated output before any manual activation of a Rust installation. There is no live Core mutation.

## Safety and limitations

- Inputs bounded at 8 MiB each. Nested duplicate JSON keys, malformed JSON and trailing bytes are rejected. Current `presets` object only; older Python `custom` and list formats are not yet supported.
- Preset IDs must be positive, unique numerically (including `18` versus `018`), agree with their optional embedded IDs, and not collide with Python template IDs. A source with any incompatible preset or unsupported converter feature **cannot publish partially**. Explicit blockers require a future conversion extension, not an implicit skip.
- Outputs include `presets.json`, `preset-credentials.json`, template descriptors and byte-for-byte copies of both original Python input files. Secrets are not embedded in the Rust preset catalog. On Unix, the staging directory is mode `0700`, files are `0600`. Windows ACL inheritance and GUI end-to-end require separate verification.
- All files are written into one random private temporary sibling directory, flushed, then published by a directory rename; on normal writing/validation failures the temporary directory is removed. On Unix, the directory is also synced. No Rust profile, old Python installation, live secret store or existing destination directory is overwritten deliberately.
- This is offline-only; a concurrent external writer that races with the final rename is not protected by a cross-process CAS. No power-loss recovery or filesystem-agnostic atomic-no-replace guarantee is claimed. An interrupted process may leave an orphan hidden staging directory, never a partially published target under normal operation.
- Python preset custom display `order`, legacy formats, non-convertible provider fields, active preset selection, historical IDs, external credentials policy, fallback chains and unsupported model options are **not** migrated into the active Rust application. The original inputs are retained precisely for later recovery and manual comparison. `PythonPresetStagePreview` flags that Python order is present but cannot be represented by current Rust catalog metadata.

**Next:** integrate reviewed staging with a unified installer transaction, preserve preset ordering if Rust UI requires it, test Windows permissions and blocked/legacy fixtures, and only then consider full issue #45 acceptance.
