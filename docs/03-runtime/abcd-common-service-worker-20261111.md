# ABCD: common Core service-worker dispatch boundary

## Decision

The application configures AI services, not execution-language modes. A service
has at most one running model. Model identity and transport-specific startup
remain infrastructure inputs; the Core dispatch contract does not branch on the
implementation language.

### Dependency / ownership graph

```text
ApplicationLifecycle (orchestration)
    -> ServiceWorkerRegistry (one service slot + transition/inference gate)
         -> WorkerClaim (the existing process owner's claim; one per service)
         -> ServiceWorker<typed operation port> (non-owning binding)
              -> WorkerInvocation (temporary read permit for one operation)
                   -> backend's typed operation (TTS / ASR / Embed / Rerank)
```

`ServiceWorkerRegistry::claim` is made **before** a process starts, and the
corresponding `WorkerClaim` is dropped **after** its process has terminated.
The claim rejects a second active model for the same service even if its
implementation or revision differs. `transition_gate(service)` is exclusive and
waits for all in-flight calls on that service; other services have independent
gates and continue concurrently.

`ServiceWorkerRegistry::connect(expected, backend)` binds an existing typed
operation port to the **exact active claim generation**. It does not spawn a
process and does not own a second claim. It fails if the requested model is not
active. A call to `ServiceWorker::begin` acquires the per-service read gate and
validates that generation again, after waiting for any transition.
`WorkerInvocation::dispatch` keeps that gate during the entire asynchronous
operation and verifies generation before returning its output. If an operation
fails, it does not silently switch models or implementations. Dropping an
in-progress invocation (cancellation, error, panic unwind) releases the gate.

A newly started process running the **same** model gets a different claim
generation. An old bound `ServiceWorker` cannot use it; explicit connection of
the new backend is required. This prevents ABA across restarts.

### Typed ports

- TTS: the existing `TtsWorker::synthesize` port; `RoutedSpeechVoice` binds
  local model-backed calls through the common `ServiceWorker` API.
- ASR: the existing `AsrWorker::transcribe` port; all model-backed calls use
  the same API and active-session admission.
- Embedding: `EmbeddingProvider` is the operation port; `ServiceEmbeddingProvider`
  only adapts that typed contract to `ServiceWorker`.
- Reranker: `RerankProvider` similarly adapts `score` to `ServiceWorker`.
- `WorkerImplementation` and model catalog data are not needed by the common
  service-worker ownership or dispatch API; existing construction/selection
  stays behind the infrastructure adapter for now.

The active Timeline knowledge-dispatch lease is orthogonal to the worker
invocation. A semantic request must hold **both** its SQLite-authoritative
source lease and the service-worker invocation while calling an external
provider. The worker layer never changes retrieval semantics or source access.

### Transition invariants

1. At most one process/model per service; service slots are independent.
2. No service transition overlaps an admitted inference on that service.
3. No backend method is entered before admission under the service gate.
4. A result from a retired generation is never returned to a caller of the
   old binding, even if model name and revision are unchanged.
5. Cancellation releases operation admission; the process owner alone manages
   worker termination and claims.
6. A failed call never falls back to another implementation automatically.

### Remaining work / deliberately excluded

- The concrete native/Python worker processes are still constructed and
  supervised in their existing lifecycle owners. This iteration unifies Core
  **operation dispatch**, not process creation protocols or installation flows.
- Existing Telegram/API TTS transports are not local model-backed workers;
  their current settings/transport behavior is retained. They do not claim a
  model-worker process slot.
- Generic typed worker factory and external native Embedding/Reranker operation
  implementations remain future transport modules, not new Core branches.
- No changes here to Slint model catalog behavior, package signing, SQLite
  knowledge-dispatch reservation, or user-selected model policy.
