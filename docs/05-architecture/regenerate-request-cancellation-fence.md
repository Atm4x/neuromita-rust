# B66 — Request cancellation to durable Regenerate terminal

A `RequestHandle` for an internally admitted Regenerate carries a narrow `RegenerateCancellation` owner contract. Ordinary requests carry no cancellation fence and retain their existing lifecycle.

1. Cancellation checks the request's volatile terminal/applied state without retaining any request or registry lock.
2. The application owner writes `RegenerateAbandoned` to the **original admitted child**, before setting the request's volatile cancellation token. The branch-anchored operation is valid even after selection invalidation; it is not a selection change. The typed `RequestId`/child receipt and atomic branch-head CAS prove the target. `RegenerateAbandoned` and `AcceptedReply` cannot coexist in transient or persistent Timeline.
3. After the native append commits, cancellation updates `RequestRecord` and notifies the worker. If the native write fails, `RequestHandle.cancel` does **not** report a successful cancellation and leaves its token untouched. The existing `CancelResult` has no unavailable/error variant; `TooLate` is a conservative refusal, not necessarily proof that acceptance occurred. A richer typed cancellation result is a separate public API change.
4. If acceptance had already committed, abandonment is refused and the accepted answer and effects remain unchanged. If abandonment commits first, the late accepted reply and its effects fail the same native domain validator inside SQLite `BEGIN IMMEDIATE`.
5. Processing errors (`TextGenerationFailed`, `InternalError`) attempt the same durable terminal before their volatile completion. Failure of this attempt is logged and is **not** a guarantee of durable terminal state. A process crash between failure and repair may still leave an unknown receipt. No automatic replay follows from `DispatchUnknown` or `MayHaveDispatched`.
6. The actual external provider may already have been called. This protocol suppresses Timeline acceptance, not external side effects or provider network operations.

## Lock ordering

Neither `RequestRecord::inner` nor `RequestRegistry::records` is held while entering Timeline. The registry gathers cancellation handles before iterating them. The SQLite native append serializes cancellation versus acceptance. `RequestRecord` is updated only after Timeline returns.

## Evidence

The B66 app-level tests cover request-handle cancellation before and after dispatch, the `Accepting` phase, already-accepted replies, shutdown `cancel_all`, failure terminalization, ordinary requests, SQLite error injection and retry, independent SQLite-owner cancellation/acceptance, selection invalidation, reopen and native archive. Existing Timeline tests cover private causal identity, effect-batch rejection, duplicate acceptance and A→B→A transitions. No GUI/Slint or Python legacy changes are made.

## Remaining

The protocol is not proof that an external provider call was cancelled, nor an exactly-once delivery service after crash. `RequestHandle.cancel()` still lacks a typed `Unavailable` result. Crash-recovery policy for a stranded admitted request and complete acceptance of a public Regenerate command remain separate gates.
