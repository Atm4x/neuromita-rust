# B65 — durable pre-provider dispatch fence

This is an **internal backend protocol**, not a public Regenerate action and not evidence of external provider execution.

- `RegenerateAdmissionReceipt` remains the immutable authorization identity at fork/selection. Admission alone does not mean the worker started or invoked the model.
- After context and knowledge selection but **before** `ResponseGenerator::generate_observed`, the request handler calls the injected `ProviderDispatchFence`, an application-owned adapter to `SessionTimeline::permit_regenerate_dispatch`.
- The owner checks the selected branch/generation and canonical receipt `RequestId`, then appends exactly one observerless `RegenerateMayHaveDispatched` fact. It carries only the request UUID; no prompt, credentials, provider output or private content. All legitimate writes use the common domain transition validator.
- SQLite `BEGIN IMMEDIATE` simultaneously checks actual selected generation, current branch head, existing marker, abandonment and accepted reply, and persists the fact, operation receipt and revision. Transient append and SessionArchive import use the same validator. A second independent SQLite owner cannot issue a second permit.
- If the write fails, **no provider call begins through this handler path**. On crash after the fact but before/during/after the provider call, recovery only knows `MayHaveDispatched(EventId)`; it never proves a real network call. Neither this state nor `DispatchUnknown` permits automatic requeue.
- `RegenerateAbandoned` may follow a dispatch permit; it suppresses future acceptance but does not cancel a running external provider. If abandonment precedes permit, the permit is rejected. An already accepted reply also rejects a new permit. The canonical accepted result remains the only `AcceptedReply` evidence.
- The fence is checked on the existing request worker's ordinary response-generator entry point; no UI/Slint callback or replay path was added. It does not guarantee *exactly one HTTP call* inside provider internals, tool invocations or provider retries. Cancellation or worker failure can leave an admitted request with `DispatchUnknown`, or a permitted request with `MayHaveDispatched`, even when the provider never actually ran.

Acceptance boundaries still unresolved: durable immutable generation payload/provider-level idempotency, external cancel acknowledgements, provider exactly-once, effect reconciliation, lifecycle recovery operators and UI confirmation. Issue #36 remains open.
