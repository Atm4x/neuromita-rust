# D10 — Python API preset selection and order review

Source verified read-only: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`, `src/controllers/api_presets_controller.py` (`_ensure_default_preset`, `_normalize_presets_order`, `_load_presets_only`, `_save_presets`). This only covers the current object-based Python import supported by D5–D9, **not** legacy list/custom formats.

Python stores the last selected **configured user preset** in `Settings/settings.json` as `LAST_API_PRESET_ID` and the custom display order in `Settings/api_presets.json` as `order`. Templates are not eligible active preset IDs. Python normalizes its display order by keeping valid IDs in user-defined order, dropping duplicates/unknown IDs and appending missing IDs in ascending order; it may select the first ordered preset when the prior selection is missing or stale.

## Offline review, no activation

```text
cargo run --locked --offline --example import_python_presets -- \
  /absolute/Python/Settings/api_presets.json \
  /absolute/Python/Settings/api_templates.json \
  /absolute/Rust/UserData/review-new \
  --python-settings /absolute/Python/Settings/settings.json \
  --existing-dir /absolute/Rust/UserData \
  --python-schemas-dir /absolute/Python/Settings/APIModelSchemas \
  --dry-run
```

Remove `--dry-run` to publish a **new directory** containing existing D6 review files, `python-source-settings.json` (byte-exact, potentially secret-bearing), and `python-preset-preferences-review.json` (IDs only). Existing Rust `settings.json`, live selection, character mappings, catalog and credentials are never changed by this command. The source backup and review live within a Unix-private staged directory (0700; files 0600).

Review validates that `LAST_API_PRESET_ID`, if present and not null, resolves to a *successfully converted Python user preset* in the same source snapshot. It refuses template IDs, unknown IDs, invalid numeric types, malformed and duplicate JSON keys and a Rust settings envelope. It distinguishes an absent key from explicit JSON null; in both cases **no active selection is inferred**.

The explicit Python `order` is interpreted only for converted user IDs. Missing IDs are appended in ascending order, matching Python. Unlike Python's permissive runtime normalization, the offline importer **rejects duplicate/unknown/invalid IDs** instead of silently dropping them. A missing/null order is reviewed as ascending ID order and marked non-explicit. Existing Rust-only IDs do not become part of the Python-specific display order.

`python-preset-preferences-review.json` is an **audit artifact**, not a new Rust schema or source of truth. It is deliberately ignored by D7's two-file activation. Selection is owned by `AppSettings.presets.active` in **a third file** (`settings.json`), while order has no Rust persisted representation. Updating those preferences requires an independently designed three-document ownership/recovery contract and UI ordering support; do **not** claim D7's two-file journal makes this atomic. No automatic template fallback or Python settings overwrite.

No Windows ACL/runtime or power-loss certification is claimed.
