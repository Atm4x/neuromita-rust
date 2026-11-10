# Atomic checkpoint branch selection — B58 owner slice

Status: **implemented primitive, not an accepted Regenerate operation**. Source base: B57 `0b37fd54b93a093b2288737f014876cc383506c5` / tree `6ff5acbcdb695943840ad55e67945d738b6053b6`.

## Invariant

`SessionTimeline::fork_and_select_checkpoint(checkpoint, child, expected_selection, expected_parent_head)` accepts only an existing `Checkpoint` in the currently selected parent branch. An arbitrary player `EventId` **is not** a valid fork anchor. The method does not submit a generation request, duplicate the player message, or alter previous branches. It produces a new branch whose visible world is the prefix through the checkpoint, plus the branch marker, and advances the selection generation exactly once.

Before a mutation, the session owner verifies the current selection (including its durable counterpart for SQLite), the parent, head, candidate child identity, checkpoint kind, generation overflow, and every character's prefix knowledge epoch. The transient path then performs an infallible selection installation after `TimelineStore::fork`. Other threads cannot interleave through `&mut SessionTimeline`; its caller must maintain the existing owner mutex discipline.

For SQLite, the selected-repository handle executes `fork_and_select_checkpoint` in **one** `with_write` / `BEGIN IMMEDIATE` transaction: compare the persisted `TimelineSelection` and parent head, reject child identity reuse, apply `TimelineStore::fork`, persist child facts and lineage, CAS-select the child, advance the timeline revision, then `COMMIT`. Any invalid precondition, insert/update trigger failure, or revision failure before commit rolls back both persisted changes. The preexisting ordinary `select_branch` method now shares the same transaction-local CAS implementation to avoid semantic drift. The separate fact-repository handle observes the committed state on its next read; the session owner installs local selection and broadcasts changes **after** durable commit.

The primitive preserves source event identity, timestamp, payload, observer sets and causal links via prefix sharing; it does not copy a new player event. Downstream resets, effects, messages and assets remain on the original parent branch but are not in the child's selected world. The test fixture covers a payload carrying a media reference; this is **not** a proof of real AssetStore garbage-collection safety or multimodal capture.

## Executable tests

- Transient owner prefix preservation, selection fencing, observer privacy, and downstream reset exclusion.
- SQLite owner reopen with the child still selected and source input still accessible.
- SQLite source payload/identity preservation, observer privacy, downstream reset exclusion, and both parent and child readable after reopen.
- SQLite selection-update trigger failure **after child persistence**, proving no orphan child, no selection change and no revision bump after reopened inspection, plus successful retry.
- SQLite stale parent head, A→B→A selection revision mismatch, duplicate/double attempt rejection without a new branch.

## Remaining for accepted Regenerate

A normal accepted assistant reply has no mandatory checkpoint at its preceding player message. The B57 preview already identifies a canonical accepted receipt and original `ReplyTo(EventId)`; it remains read-only. The future owner protocol must (1) introduce an admissible historical prefix anchor without silently broadening arbitrary forks; (2) bind the immutable accepted receipt, real multimodal input, observer privacy and typed `ChatOrigin` revision; (3) acquire queue permit, settings and prompt before an **atomic combined mutation plus registration/admission**, including failures and cancellation; (4) prove single accepted response and late-result rejection; and (5) wire snapshot-bound confirmation and real headless Slint actions. Do not implement via `MessageHidden`, text replay, or a fabricated UI action.
