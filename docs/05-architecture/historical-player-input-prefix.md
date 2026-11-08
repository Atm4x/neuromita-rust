# B59 — historical player-input prefix, without Regenerate admission

The fork anchor is an existing canonical `PlayerMessage` EventId addressed to the selected character. Its original event identity, content, media references, timestamp and observer set are inherited by lineage, not replayed or duplicated. The child receives only a synthetic `BranchCreated` fact immediately after that input. Parent downstream facts, reset and game effects stay in the parent world.

`TimelineStore::fork_at_player_input` requires an authentic player actor and recipient witness and an input still visible to that character. Hidden, foreign, fabricated or non-player anchors are rejected. This is distinct from `TimelineStore::fork` which retains checkpoint semantics and its old idempotent retry behavior.

`SessionTimeline::fork_and_select_player_input` locks the owner and guards selection, branch/head/generation and input visibility. The persistent path uses `SqliteTimelineRepository::fork_and_select_player_input`, sharing the `BEGIN IMMEDIATE` fork+selection transaction with the checkpoint path. Failed selection writes roll back child creation and timeline revision. Two owners trying the same generation cannot select two children; branch A→B→A still advances generation. An append after prepared head invalidates the fork. Notification channels change only after commit.

Persisted `checkpoint_id`/`BranchInfo.checkpoint` is a legacy database column for a fork anchor. The new branch marker unambiguously carries `input_event_id` and `character_id`; checkpoint branches retain their original marker. Native session archive validation checks the exact marker payload, source event kind, actor, recipient witness and parent prefix sequence. Python external archive v1 remains checkpoint-only and is not modified.

## Verified boundaries

Local Rust 1.98.1 tests exercise transient owner prefix, privacy and hidden rejection, SQLite reopen, A→B→A, rollback with injected selection-trigger failure, competition between persistent writers, and native archive export/import with tampered recipient rejection. The original B58 checkpoint tests remain unchanged.

**This is not accepted Regenerate.** No receipt-bound `RequestId` admission, queue permit, immutable settings/prompt capture, cancellation, accepted effect migration or UI callback is created. Regenerate must combine accepted receipt validation, snapshot-bound user confirmation, source selection, queue admission and durable fork into one safe protocol. A standalone input fork is owner-internal and cannot by itself guarantee that failed queue admission leaves Timeline unchanged. Do not expose this method to UI or claim issue #36 complete.
