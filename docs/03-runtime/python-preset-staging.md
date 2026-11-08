# Python API preset staging — Agent D (2026-11-07)

**Scope:** safe, *standalone* conversion of the current Python `Settings/api_presets.json` and `Settings/api_templates.json` into a **new**, empty output location. Not an in-place migration of an installed NeuroMita profile; issue #45 remains open.

Reference Python source: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`, `src/controllers/api_presets_controller.py`, which persists `{"presets": {"id": ...}, "order": [...]}` and `{"templates": {"id": ...}}`. Rust already owns a validated `PythonPresetImport` mapper under `generation`; this change builds a non-destructive publication boundary around that existing converter, without changing provider settings semantics.

## Invocation

```text
cargo run --locked --offline --example import_python_presets -- \
  /path/to/Python/Settings/api_presets.json \
  /path/to/Python/Settings/api_templates.json \
  /absolute/portable/UserData-import-review \
  --python-schemas-dir /path/to/Python/Settings/APIModelSchemas --dry-run

cargo run --locked --offline --example import_python_presets -- \
  /path/to/Python/Settings/api_presets.json \
  /path/to/Python/Settings/api_templates.json \
  /absolute/portable/UserData-import-review \
  --python-schemas-dir /path/to/Python/Settings/APIModelSchemas
```

The example prints preset IDs, schema counts, source inventory SHA-256, and the names of files requiring separate migration. The exact schema directory path is mandatory and must be the `APIModelSchemas` sibling of the supplied Python preset file; an absent path is recorded as verified absence, not silently replaced with an empty directory. No schema file is installed in the active Rust settings. No API keys, template private fields or blocker values are logged. `--dry-run` performs validation and preview without creating any files. The target must **not already exist**. The intended workflow is to stop both applications and review generated output before any manual activation of a Rust installation. There is no live Core mutation.

## Safety and limitations

- Inputs bounded at 8 MiB each. Nested duplicate JSON keys, malformed JSON and trailing bytes are rejected. The current and source-backed historical Python layouts are supported as documented in `python-preset-offline-activation.md`.
- Preset IDs must be positive, unique numerically (including `18` versus `018`), agree with their optional embedded IDs, and not collide with Python template IDs. A source with any incompatible preset or unsupported converter feature **cannot publish partially**. Explicit blockers require a future conversion extension, not an implicit skip.
- Outputs include `python-source-schema-inventory.json` (filenames, raw-byte hashes and classifications, never schema content), `presets.json`, `preset-credentials.json`, template descriptors and byte-for-byte copies of both original Python input files. Secrets are not embedded in the Rust preset catalog. On Unix, the staging directory is mode `0700`, files are `0600`. Windows ACL inheritance and GUI end-to-end require separate verification.
- All files are written into one random private temporary sibling directory, flushed, then published by a directory rename; on normal writing/validation failures the temporary directory is removed. On Unix, the directory is also synced. No Rust profile, old Python installation, live secret store or existing destination directory is overwritten deliberately.
- This is offline-only; a concurrent external writer that races with the final rename is not protected by a cross-process CAS. No power-loss recovery or filesystem-agnostic atomic-no-replace guarantee is claimed. An interrupted process may leave an orphan hidden staging directory, never a partially published target under normal operation.
- Python preset custom display `order`, legacy formats, non-convertible provider fields, active preset selection, historical IDs, external credentials policy, fallback chains and unsupported model options are **not** migrated into the active Rust application. The original inputs are retained precisely for later recovery and manual comparison. `PythonPresetStagePreview` flags that Python order is present but cannot be represented by current Rust catalog metadata.

**Next:** integrate reviewed staging with a unified installer transaction, preserve preset ordering if Rust UI requires it, test Windows permissions and blocked/legacy fixtures, and only then consider full issue #45 acceptance.

**Existing Rust profile review (D6):** [python-preset-existing-profile-review.md](python-preset-existing-profile-review.md) documents the opt-in `--existing-dir` merge of validated snapshots into a new directory, preserving existing/retired preset IDs and separate credentials without live activation.

## D35 — Read-only Python `APIModelSchemas` gate

Reference: pinned Python `src/model_settings/{repository,schema,bundled_json_compat,builtins}.py` at `5f3d061f523d9abefa6a264bb2953048a935fa65`. Python's `SchemaRepository.catalog()` ignores historical `g4f` schemas, identifies **known unmodified** factory JSON by 14 exact SHA-256 hashes across 12 IDs after validated canonical JSON serialization, and may apply a user-edited external schema when its revision and dialect are compatible with the current built-in. An arbitrary same-ID file is **not** necessarily the built-in.

The reviewed directory must be passed explicitly, even if absent. The Rust inspection is read-only and bounded (128 JSON files, 1 MiB per file, 8 MiB total); it rejects symlinks, unsafe directories and unreadable JSON entries. The review records the presence bit, exact raw-byte file SHA-256 values, sorted filenames and a digest of the full inventory. Only an exact recognized factory fingerprint or the Python-ignored `g4f` dialect may pass this **selective** stage. Unknown/edited/invalid schema files fail closed and are not copied into active model schemas. Other non-JSON files are not considered by the source Python `*.json` loader.

The stage re-scans the catalog before writing the private sibling directory and immediately before publishing it; any appearance, disappearance or byte-level change refuses publication. `python-source-schema-inventory.json` is a non-authoritative audit artifact; it does **not** back up the external schema bytes. Preserve the full Python `Settings/APIModelSchemas` directory separately for a complete backup. An unsupported file requires a future typed model-schema converter, not an implicit skip.

The scan protects against ordinary stopped-app source drift, not malicious writers swapping symlinks between filesystem calls. No interprocess lock exists with Python; actual cross-filesystem CAS, ACL inheritance, GUI UX and physical power-loss remain uncertified. Existing lower-level `PythonPresetStage::stage_new_directory()` intentionally remains an unaudited API for older tests and internal staged flows; a migration using it **cannot** claim the external-schema inventory gate.
