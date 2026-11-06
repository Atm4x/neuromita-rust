# C2 — Stable fatal IPC diagnostics on repeated requests

Scope: partial issue #24, stacked on the shared-worker and streamed-generation fix.

After an IPC transport failure has marked a worker generation as `Failed`, further requests to that same retired worker can only return the generic `worker requires restart` error. Rewriting the original failure reason in the observable status would hide the causal error, mislead the user, and cause unnecessary watch updates. The supervisor now preserves a failed generation's first recorded fatal cause. Nonfatal plugin errors remain service-scoped, and repeated identical nonfatal errors are also not published as spurious status changes. The watch sender uses `send_if_modified` to avoid duplicate notifications.

The shared worker regression observes the snapshot after killing a real Python child and verifies both TTS and ASR retain identical original failure reasons after a subsequent rejected request, without a watch change. This does not imply automatic restart; explicit stop/start and reactivation remain separate operations.
