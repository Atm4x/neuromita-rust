# B63 — restart-safe Regenerate reconciliation, not dispatch replay

A committed `RegenerateAdmissionReceipt` proves admission in a historical fork, not that a worker started, completed or was cancelled. `SessionTimeline::reconcile_regenerate(child, request)` reads that canonical marker, requires its exact `RequestId`, and returns one of two durable outcomes:

- `DispatchUnknown`: no canonical accepted reply is committed in the specified child branch. This includes crashes before queue registration, during provider I/O, cancellations without a durable terminal fact, and lost callbacks. **Never** infer `PendingDispatch`, `Cancelled`, `Abandoned` or permission to retry from this state.
- `AcceptedReply(EventId)`: exactly one canonical character message bearing this `RequestId` exists in the child and its single `ReplyTo` link and payload identify the same original player input in the admission marker. A reply from the parent, another branch/character, or with a different cause cannot satisfy this result. Multiple accepted messages with the same identity produce `Conflict`, not an arbitrary first winner.

The snapshot includes the current selection and generation. Under SQLite this comes from the durable `timeline_active_selection`, not the owner instance's possibly stale workspace. Selection may change after the read; callers **must not** treat reconciliation as an authorization fence. All future writes require the native admission/selection CAS.

The generation acceptance path now checks the durable receipt before constructing a reply/effect batch. If the accepted `RequestId` is the child's Regenerate admission ID, its intent and selected branch must match the original input, character and child branch, and an already-committed reply is rejected. The existing guarded native append checks selection, head, knowledge epoch and accepted-reply revision inside `BEGIN IMMEDIATE`; a competing SQLite writer cannot commit a second reply with the stale pinned turn. The original parent facts and media are not mutated.

## Proved slice

- Read-only/idempotent unknown-result reconciliation with no new Timeline fact or queue request.
- Identity mismatch and invalid causal acceptance rejected; duplicate accepted facts detected.
- Same-ID runtime acceptance commits once, a second attempt cannot append additional effects.
- SQLite reopen and A→B→A selection generation remain distinguishable.
- Competing SQLite owners attempting acceptance use guarded append and at most one succeeds.

## Explicitly not implemented

There is no durable `Dispatched`/`Cancelled`/`Abandoned` lifecycle or restart-safe replayable frozen configuration. In-flight external provider calls cannot be deduced from SQLite alone. This API does **not** automatically retry, restart a worker, or mark any incomplete admission as failed. Completion, external effects/asset retention, and durable cancellation still need independent reconciliation and cancellation fencing. Public UI Regenerate stays disabled.
