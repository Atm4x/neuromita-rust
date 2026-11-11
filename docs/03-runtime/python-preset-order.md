# Persisted user preset ordering (D12)

The canonical user-preset display order is part of the existing `presets.json` payload, **not** a separate `AppSettings`, UI-only array, or second database. The existing `schema_version` remains unchanged. Optional `order` is a list of numeric user `PresetId`s.

- Old Rust catalogs with no `order` load in ascending ID order. Missing IDs in a partially populated older catalog are appended deterministically on read. Duplicate and unknown IDs are rejected, never silently dropped.
- `PresetCatalog::reorder(&[PresetId])` requires each persisted preset **exactly once**, rejects runtime-only and unknown IDs, and uses the same storage/publish revision path as CRUD. A no-op reorder does not rewrite storage or advance the notification revision.
- New presets append; deletion removes only the deleted ID; edits and reopen retain the order. IDs are stable and never reused. Presenter snapshots and `list()` follow the same `order` field.
- Slint's API preset card menu exposes **Move up** / **Move down**, routed through the normal GUI command and presenter. This is a move operation, not drag/drop; search/filter does not alter underlying order.
- Python `Settings/api_presets.json` `order` is normalized against converted **user** presets; missing ones are appended in ascending ID order, matching the verified source semantics. Unknown/duplicate/malformed entries fail the staging preflight. Existing Rust IDs preserve their current relative order, imported Python IDs append in their Python order.
- The D7/D11 offline journal copies the full `presets.json` byte snapshot, so order and credentials remain covered by the existing recovery contract; no fourth file or new commit marker was introduced. Python active selection remains a separate `settings.json` document and is handled only by the explicit D11 three-document path.

Limits: only cooperative stopped-profile activation is supported; OS file locking and Windows power-loss durability require separate testing. The Slint GUI compile check and GUI interactions require an actual GUI environment; successful headless unit tests do not certify Windows GUI behavior.
