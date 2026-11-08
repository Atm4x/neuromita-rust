# C3/C4 — durable Core operation read model (2026-11-08)

## Scope and ownership

`InstallOperation` remains the **only** writer of the Core-owned production installation journal at `AIData/operations/production/<private-plan-fingerprint>.jsonl`. The new `InstallOperation::inspect` and bounded `inspect_all` are read-only projections of that same event stream. `AiInstallOperationPresenter` exposes those projections; it does not introduce a second queue, ledger, state machine, catalog, or installation API. This change deliberately does **not** modify the Slint AI Hub page or the independent restricted-wheel operation strip.

Each new `Begin` event contains a bounded public identity for selected models: `AiService`, plugin ID, package revision, and optional asset revision. Raw model settings, authorization tokens, plugin source, Python install plan, approval sets, and private plan fingerprint are not included in the read model. Old v1 journals without model metadata remain readable. Resuming an old incomplete journal without model identity preserves that legacy record; no identity is inferred or rewritten.

Stage transitions remain `Pending → Started → Verified` with a final `Completed` journal event. `Verified` here means **the writer recorded stage verification at the time**, not that today's filesystem still has the receipt. `RecordedComplete` means the durable journal recorded successful composition, **not** that Python worker initialization or inference succeeded. Every projection sets `needs_receipt_recheck = true`; authoritative backend, overlay, asset receipts and runtime status remain independent Core owners. `ModelReady` must still come from `RuntimeSnapshot`, not this journal.

## Recovery and failure semantics

A reader inspects the existing OS ownership lock without creating or taking ownership of the operation. When an active writer owns the lock, an unfinished operation projects `Running`; after the writer disappears, it projects `Interrupted`. An abandoned `Started` nonrepeatable stage projects `NeedsReceiptResolution`: the Core installer first checks the exact installed immutable asset receipt; manual intervention is required only when that authoritative verification cannot resolve the interrupted hook. This read model does not claim automatic resumption.

On a production failure **after the whole-selection preflight has opened an operation**, the installer durably appends a sanitized `Failed` event class (`source_changed`, `backend`, `overlay`, `assets`, `composition`, or `other`). Error messages, command lines, subprocess stdout, user settings, file source paths and secrets are not persisted. An interrupted process may have no `Failed` event, and bootstrap/preflight failures happen before operation journal creation; no failure event is promised for them. Successful subsequent `Verified`/`Started`/`Completed` transitions clear the last failure classification.

`inspect` never repairs a trailing torn JSONL record. It ignores only the incomplete trailing event, returns a `journal_has_torn_tail` flag, and makes no filesystem writes. The writer's existing recovery path may truncate and fsync that torn tail after acquiring ownership. Complete malformed events, invalid stages/model identities, mismatched active preflight, symlinks, invalid lock file metadata, and oversized records fail closed. Read-only inventory is bounded to 128 journals and 512 directory entries, returning neither private fingerprint keys nor a partial “healthy” list when one journal is corrupted.

## Tests and nonclaims

Executable tests cover active in-process and cross-process lock observations, killed OS child → interrupted, unchanged journal bytes on read, UUID continuity, missing history, recorded completion without ModelReady, identity changes on an incomplete operation, invalid complete event, torn trailing bytes, symlink protections and safe failure categories. Production installer integration tests exercise actual production `prepare` under the local mock managed-uv fixture: successful recorded ASR selection and an overlay install failure that persists a sanitized error category.

No real CPU ASR model installation/inference, official uv network failure, Windows E2E/power-loss, crash-safe cancellation, cross-service atomic commit, UI install click-through or automatic user-facing progress percentage is claimed. No implementation is added for a cancel button; cancellation availability cannot be inferred from the journal. Native GUI typechecking, if successful, is compile-only.
