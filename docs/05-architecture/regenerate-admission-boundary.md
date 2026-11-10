# B61 — guarded internal Regenerate admission boundary

This is a **backend-only, crate-private** admission path. It is deliberately not an accepted public Regenerate feature, and no UI/Slint action may call it until the durable receipt and restart semantics are complete.

## Ordering and owner contract

1. `ChatApiState::regenerate` constructs a normal `Reply(original_player_input)` generation command without re-publishing text, attachments, or the old assistant reply. The caller supplies a canonical accepted `RegenerateSource`, original `TimelineFence`, and a new unique child `BranchId`.
2. `send_with_regeneration` checks the character's queue and obtains a **reserved** mpsc permit; it captures immutable settings snapshot, assigned preset and prompt before the admission lock. Closed/full queue, missing presets/prompts, and stopping fail before Timeline changes.
3. Under the shared accepting mutex, `TimelineRequestIngress::admit_regenerate` acquires the Timeline owner mutex and validates exact fence branch/selection/head. `SessionTimeline::fork_and_admit_regenerate_source` revalidates accepted reply `RequestId`, assistant event, latest visibility, canonical causal `ReplyTo` and original player input. The child's context epoch is read from the canonical input prefix **before** any fork mutation.
4. The native historical player-input fork and selected branch CAS are performed via B59/B58 (SQLite `BEGIN IMMEDIATE`, single transaction with revision). Both are guarded by the same expected selection and head. On failure no branch/selection change is committed.
5. The successful owner call has no fallible read *after* the selected fork. It returns `TimelineAdmission` for the child's exact selection and source-prefix knowledge epoch; the already reserved queue permit and frozen configuration are registered once and sent before releasing the admission mutex.
6. `publish_conversation_message` now keeps the same admission mutex through Timeline publication, like `publish_message`, so an incoming player turn cannot slip between a Regenerate admission's selection switch and the turn's branch resolution.

## Executable proof envelope

- Queue full/closed, missing preset, stale owner head: no selected child, no queue admission.
- Success: exactly one request ID queued with frozen settings/preset, `Reply(original_input)` intent and correct child admission; original input retained, old accepted reply excluded.
- Two concurrent Regenerate calls: one winner and one stale rejection; no second queued ID.
- Concurrent player conversation event: linearized before or after Regenerate; no dropped player event and no publication on an obsolete branch chosen between check and ingress.
- SQLite reopen: selected child, source EventId and its knowledge epoch recovered. Injected selection-update trigger: transaction rollback, no child and no registered request, then retry succeeds.
- Cancellation **after** admission: request may be cancelled, but already committed historical branch remains; not confused with a pre-admission rejection.
- Existing B58/B59/B60 owner/storage tests remain in the library regression.

## Explicitly **NOT** proven / blocked

- **Process crash gap:** there is still no durable Regenerate admission identity/receipt atomically persisted with selected fork. If the process dies after SQLite commit but before registry registration / permit send, the child may be selected without a running request. This is why the entry point is crate-private, not exposed via public ChatApi/presenter/UI.
- No restart-safe idempotent replay/reconciliation of that admission; no way to prove a lost queue consumer will be recovered. A durable receipt or command ledger needs to be written in the same SQLite transaction and reconciled on startup.
- No complete late provider acceptance/cancellation and effects proof beyond existing TimelineAdmission checks; no claim of exactly one generated assistant reply, or effect/asset collection safety.
- External Python EventObserved identity/observer privacy, asset GC roots and camera/screenshot capture are outside this boundary.
- Local tests skip the known Python 3.12-only installer fixture when Python 3.12 is unavailable; do not claim full unfiltered PASS.
