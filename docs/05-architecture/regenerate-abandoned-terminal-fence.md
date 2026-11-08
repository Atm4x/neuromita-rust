# B64 — Regenerate terminal abandonment (backend only)

`RegenerateAdmissionReceipt` proves one durable historical-input fork and its queued `RequestId`. It does **not** prove provider dispatch. On uncertain shutdown, recovery continues to return `DispatchUnknown` and must never automatically requeue the request. An owner may instead make the explicit terminal decision to **abandon acceptance** for that already-admitted request.

A `RegenerateAbandoned` fact contains only the admitted `request_id`, has no actor, observers, causal links, or external message identifier, and is stored in the selected child Timeline. It means **future results of this RequestId must not enter canonical Timeline**. It does not cancel the external provider, undo already-committed effects, erase history, or describe provider completion. This fact is append-only and is not a fake message or mutable branch marker.

`SessionTimeline::abandon_regenerate(child, request, expected_selection)` rechecks selected branch, generation, and canonical durable receipt. A prior matching terminal fact is returned idempotently. Otherwise one append is guarded by branch head and active selection generation. The in-memory store, SQLite `BEGIN IMMEDIATE` append, and native archive receipt replay all use the same pure transition validator: an accepted answer and abandonment for the same durable receipt are mutually exclusive, and neither state can occur twice. Character-message acceptance must match the admitted character, original ReplyTo link and original input identity; forged and duplicate replies are rejected before mutation. SQLite locks extend through validation, fact/receipt persistence and revision commit. A cross-process losing writer cannot publish an accepted reply or its effects after abandonment.

The recovery API reports `DispatchUnknown`, `Abandoned(EventId)`, or `AcceptedReply(EventId)`; no value authorizes automatic replay. The immutable configuration reference is **not** a replayable provider invocation. No plaintext credentials are persisted. No UI, Slint, Presenter, Unity or legacy paths are modified.

## Proof boundary

Test transient idempotence, accepted-before-abandonment rejection, forged/foreign request, stale generation/branch, SQLite reopen and A→B→A, concurrent `Abandoned` versus acceptance by two independent SQLite owners, injected SQL abort before commit followed by retry, archive round-trip and forged terminal request import rejection, late callback zero Timeline mutation, full library regression and all-targets.

This is **not** complete Regenerate admission lifecycle: there is still no durable `MayHaveDispatched` proof, recoverable full provider/prompt configuration, provider-level exactly-once, comprehensive terminal cancellation coordination or enabled public chat action. External side effects may still occur even if their Timeline result is abandoned. Do not claim request cancellation, provider replayability or end-to-end Regenerate acceptance.
