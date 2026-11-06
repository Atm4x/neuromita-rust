# C2 — Stale streamed TTS replies after worker replacement

Scope: partial acceptance of #24; extends the generation-bound direct initialization change.

When the TTS IPC stream finishes, `AiSupervisor::synthesize_streamed_wav` now compares the responding worker's `Arc` identity with the worker currently published for the TTS service. It must never lock the replacement worker solely to validate the old stream's generation: the new worker may already be busy on an unrelated inference request. A superseded stream returns `StaleGeneration` without exposing audio or overwriting the replacement's observable health state.

A deterministic Rust/Python mock test starts a stream that blocks inside the previous worker, publishes a freshly initialized replacement, explicitly holds the new worker's mutex, releases the old stream, and requires the stale result to return before releasing the new worker. A subsequent stream on the newly selected worker confirms recovery. This verifies rejection of an old response and lack of blocking on the new worker lock; it does not implement automatic crash restart or a wider stream cancellation protocol.

The caller-facing `PythonVoiceProvider` API is unchanged. No new dependencies, plugins, path layout or storage versioning are introduced.

Shared topology crash diagnostics are also generation-scoped: one fatal IPC transport failure marks every attached service of that worker generation `Failed` with the same underlying error reason, while preserving independent split workers and nonfatal plugin failures. A mock test kills the shared child after TTS and ASR initialization, performs an IPC request, validates both failed statuses and their identical error, then stops the shared generation cleanly.

During verification, the shared-process test exposed an additional readiness inconsistency: direct initialization of a *second* service in an already running shared worker succeeded in Python while `RuntimeSnapshot` still reported that service as `Stopped` with no generation. The generation-locked publication path now attaches that newly initialized service to the currently verified shared worker generation. Existing unrelated generations cannot be overwritten, and the `ModelReady` phase still requires an actual successful Python `initialize` response. The same regression verifies both services are ready before the crash and both fail with the identical reason afterward.
