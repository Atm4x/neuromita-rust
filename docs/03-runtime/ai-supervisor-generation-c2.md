# C2 — generation-bound manual initialization

Scope: partial issue #24; stacked on the nonblocking health-poll change.

A direct `AiSupervisor::initialize` previously called Python, then reacquired whichever worker was currently in the service slot. If `stop/start` or a model reconfiguration replaced that slot after the successful IPC response, the old response could mark the replacement worker `ModelReady` even though it never initialized the model.

`call_tracked` now preserves the generation of the worker that actually accepted the request and rejects replies whose worker is no longer published. The public `call` API remains unchanged. After successful `initialize`, model state is published only under the existing activation transition lock and after verifying the same worker generation. A stale reply returns `AiRuntimeError::StaleGeneration`, without changing model selection or status. No additional service locator, event bus, or process supervisor is introduced.

Validation is the Rust library suite (including the new stale-initialization test), existing inference mock and cancellation/composition integration tests, and Python SDK tests. No physical model inference or automatic crash-restart implementation is claimed. Remaining C2 acceptance includes epoch handling in streamed WAV, partial worker-loss/recovery cases, explicit restart policy, and broader shared/split failure testing.
