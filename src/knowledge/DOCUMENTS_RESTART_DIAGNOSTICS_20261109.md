# Agent A Cycle62 — Durable Documents revision selection and failure diagnostics

## Ownership and contract

The only active Documents selection remains `SettingsApi`'s persisted `DocumentGenerationSettings` section. `DocumentRevisionApi` neither maintains a second settings database nor automatically repairs missing derived indexes. `activate` and `deactivate` remain caller-owned, revision-guarded changes. A successful `activate` is only an in-memory CAS until the caller explicitly awaits `SettingsApi::flush()`; the settings repository and application's normal shutdown own durability.

`DocumentRevisionApi::inspect(character)` is a **trusted, read-only** operation that returns `Disabled`, `Ready { revision }` or `Unavailable { settings_revision, issue }` from one settings snapshot and independently verified index/source. The bounded `DocumentRevisionIssue` distinguishes missing root/index, invalid index, stale source, wrong scope, unsafe source, invalid source and other I/O. It never embeds document contents or raw OS paths in its error classification. The existing `status` still returns typed errors on corrupt indexes; inspection is **not** admission and never converts an error into an enabled result. There is no new network endpoint or GUI.

## Verified persistence and failure boundaries

A real `ApplicationLifecycle` controlled-provider test starts with `JsonSettingsRepository`, prepares and activates two character-scoped indexes, calls `SettingsApi::flush`, inspects the saved `StoredSettings` revision and selection, shuts down, and restarts from `SettingsSource::Stored`. After restart, both characters remain ready and both generate source-attributed evidence.

When the first character's index is moved away, read-only inspection reports `MissingIndex`, normal `status` rejects the source and generation terminates before provider invocation; the settings revision and persisted JSON do not change and the independent second character remains operational. A stale CAS fails. Only explicit preparation/activation of a new revision restores generation. That selection is flushed and verified across a second restart; damage to the new index reports `InvalidIndex`, while changing the second character's source reports `StaleSource`. Both refuse provider invocation without modifying saved settings. The test is self-contained, has no live external model and does not exercise GUI.

The fault classification is observational rather than a stability guarantee: filesystem writes may race a check and a later generation attempt revalidates. The source index is derived and intentionally never trusted as canonical. There is no automatic fallback, reindex, activation, source data rewrite, durable activation prior to `flush`, OS writer exclusion, signed provenance or complete R7 user-facing error interface. Full R6 and R7 acceptance remains OPEN; the 26-slot matrix is unchanged at **16 PASS / 4 PARTIAL / 6 OPEN**.
