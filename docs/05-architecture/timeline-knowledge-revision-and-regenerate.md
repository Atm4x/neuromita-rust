# Timeline — revisioned knowledge, History Reset and Regenerate

Status: approved semantic target (2026-11-09); implementation is incremental. The pre-existing fork-based Regenerate must **not** be represented as this model until its durable admission is migrated. No UI contract is enabled by this document.

## Canonical facts and effective knowledge

`TimelineFact(EventId)` is a single historical occurrence. Its original `observers` (`seen_by`) record witnesses at the occurrence, **not a mutable list of who knows the fact today**. `MitaView(character, at_revision)` is the result of folding *all* canonical facts and the character-scoped knowledge-control events up to that revision. No operation retroactively changes earlier reads. The same EventId can be known at R150, unknown at R250 and known again at R350. A revealed old event retains its original identity, occurred-at time, content, media references and causal links, while `learned_at` becomes the reveal operation's revision/sequence.

Explicit system operations:

- `CharacterKnowledgeForgotten(character, event_ids)` revokes knowledge of these **specific canonical events** as of its own revision. This is not `MessageHidden`, which changes only the visible message projection.
- `CharacterKnowledgeRevealed(character, event_ids)` grants direct access to these exact historical EventIds from the new revision onward; it is an explicit privileged operation, **not** an automatic consequence of hearing a message mentioning them. A story is a new message event and yields knowledge only of that message.
- `CharacterContextReset` is the existing native **History Reset** event for a character. It clears that character's then-current short-term knowledge and visible context at its revision, not other characters' histories or canonical events. It is stronger than targeting a finite list with Forget. Subsequent Reveal can selectively reintroduce old facts; a new conversation or causal link never does so implicitly.

Control operations must be subject to the same transactional CAS, privacy, archive validation and memory/semantic-index invalidation as other context mutations. `at` is an **as-of read**, not time travel or moving the character into a separate branch. The control event itself is observed, but it must not expose referenced event content to other recipients. Privileged disclosure requires a trusted authorization boundary; do not expose a public API that accepts arbitrary EventIds without authorization.

## Regenerate semantic target: one shared session, no global historical fork

1. Validate the accepted assistant message, immutable request receipt, canonical original player input and its `ReplyTo(EventId)` under a stable Timeline fence. Freeze prompt/settings/queue permit *before* any mutation.
2. Determine and explicitly confirm an exact **character-scoped** invalidation set. The selected old assistant answer and any other replaced assistant messages are hidden from the relevant chat projections. Do not globally hide every future event or erase unrelated conversations with other Mitas. Character-specific Forget applies only to the selected knowledge set; it does not rewrite other characters' `seen_by`.
3. Commit visibility operations, character knowledge-control events, new request admission and an idempotent receipt in **one** SQLite transaction / single canonical timeline revision fence. If admission fails, commit none of them. The original player EventId (including media) remains the `ReplyTo` target; never resubmit its text as a new player event.
4. Generate against a **frozen MitaView at the post-control revision**. A regenerate at current knowledge state is not historically positioned at the original message; do not accidentally conflate the two semantics. User-visible confirmation must describe whether later independent knowledge is retained.
5. Accept exactly one resulting reply with effects, respecting durable cancellation, abandonment, races and late-provider results. Only derived projections are recomputed: physical Unity/game side effects already executed require separate reconciliation and must never be silently considered rolled back by Forget/Hide.

**Current implementation boundary**: B58–B74 implements durable `RequestId`, historical branch admission, one-shot dispatch, cancellation, recovery and pagination *for fork-based Regenerate*. These safety primitives are reusable, but the existing fork+selection operation is NOT the proposed character-scoped knowledge mutation. The two protocols must not be mixed or simultaneously exposed. Migration of the durable receipt to a same-branch mutation is necessary before enabling new Regenerate. Until then the new events are a foundation and Regenerate remains unaccepted.

## Required proofs

- R150 known / R250 forgotten / R350 revealed of the exact same EventId (transient and SQLite reopen, historical `at`), and no knowledge transfer through `ReplyTo`/`CausedBy` or a third party's story.
- History Reset clears preceding knowledge; later direct observations are preserved, later explicit Reveal works; unrelated characters and their media remain unchanged.
- `MessageHidden` alone cannot force Forget; Forget alone cannot pretend to undo durable game effects.
- Invalid/foreign/duplicate target payload and stale CAS reject without mutation; SessionArchive replay rejects forged operations.
- Regenerate admission, hide, forget and accepted receipt all-or-nothing; concurrency, restart, late answer, cancellation, semantic retrieval and asset roots remain correct. Never substitute another global branch fork for this contract.

## B75 implemented slice and migration gate

The new `CharacterKnowledgeForgotten` / `CharacterKnowledgeRevealed` facts have native domain validation, read-time folds, derived SQLite indexes, history-epoch invalidation and owner-scoped append; `CharacterContextReset` remains the existing History Reset. `SessionTimeline::plan_knowledge_regenerate` is a strictly read-only proposal anchored to a canonical accepted `RequestId`, original `ReplyTo`, current selection/head and effective visibility. It proposes hiding the selected character's authored visible replies from that accepted reply forward while preserving another Mita's messages. It separately identifies character-local causally linked effects for reconciliation; it does **not** undo their durable application, infer all dependent player/world events, or commit any mutations.

The current `ChatApiState::regenerate` **still follows the B58–B74 fork path**; the new read-only plan must not be wired to the queue until same-branch durable admission+knowledge/visibility mutation has been implemented and proven in one transaction (including concurrent owner failure, crash/reopen, cancellation/late accepted effects and asset/semantic retrieval invalidation). Do not prematurely treat the B75 read-only plan as an accepted end-to-end Regenerate. Existing historical fork APIs remain for checkpoint/explicit alternatives and compatibility; they are not the new default semantic design.

The older Python `Events.Character.CLEAR_HISTORY` command is a product/UI operation with its own asynchronous handlers; do not treat emitting it as equivalent to a committed native `CharacterContextReset` without a verified bridge. In native Timeline, `CharacterContextReset` is the canonical, already-existing history-knowledge-reset fact. This is *not* `MessageHidden`.
