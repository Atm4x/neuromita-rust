# Accepted-message Regenerate — B57 provenance preflight

**Base:** B56 PR #296, `a2103729457269197c8cd577dff1df6f851df1fc`.
**Scope:** read-only typed preview and executable integration regression, not accepted-message Regenerate execution.

## Active Python reference (Atm4x/NeuroMita main, 2026-11-08)

- `src/ui/chat/message_widget.py` exposes `Regenerate` on assistant messages and `Regenerate from here` on user, assistant and system messages.
- `src/controllers/gui/chat_message_actions_view_model.py` dispatches `RegenerateChat` and `RegenerateChatFrom` separately.
- `src/controllers/chat_controller.py`, `_on_regenerate`: selects the last assistant message, finds the preceding user message, cuts stored history from the user turn (or just the assistant for system-as-user), and re-sends extracted text.
- `_on_regenerate_from`: identifies an explicit message ID and cuts history from the relevant user turn; a system-message target is retained.
- The Python text re-send does not preserve attachments or canonical Rust EventId; do not port its destructive mechanics.

## B57 contract

`ConversationPresenter::preview_regenerate(character, accepted_event_id, TimelineFence)`
is read-only and rejects an ambiguous, foreign, stale, non-latest, provisional,
or non-accepted assistant target. It derives the original user EventId only from
the *committed* assistant receipt's single `ReplyTo` causal link, checks its
`reply_to` payload, confirms the player fact is known to this character, and
verifies that both facts are in that character's currently visible view.
`RegeneratePreview` retains the original input, accepted response EventId,
request ID, exact timeline anchor + selection and count of *visible* downstream
message facts. The downstream count is **not** a global impact estimate.

It **does not** mutate Timeline, enqueue generation, expose a Slint action,
remove downstream events, suppress accepted effects, or assert atomicity.

## Requirements before enabling the action

1. Choose durable downstream semantics. Existing `MessageHidden` only changes
   a single observer's projected display and does not rewind accepted game,
   memory or knowledge effects. Existing `TimelineStore::fork` requires a
   pre-existing `Checkpoint`; arbitrary accepted player EventId is not a
   checkpoint. Neither may be used as a regeneration shortcut.
2. Implement the owner-controlled mutation **in the same guarded admission
   operation** after queue permit and settings/preset capture, and before
   RequestRegistry insertion; reject or rollback every remaining fallible step.
3. Preserve original player EventId and attachments, observer privacy, stored
   receipt identity, late-result rejection, cross-character isolation and
   reopen/archive semantics for both memory and SQLite.
4. Add snapshot-bound confirmation including GUI `ChatOrigin` selection
   revision; verify it again at callback dispatch (A→B→A).
5. Test concurrency, cancel, provider-late acceptance, downstream mixed-media,
   admission failure after prepare and reopened SQLite before claiming #36 complete.

This B57 source slice intentionally fails closed instead of imitating
Regenerate with `reply(text)` or changing observable history prematurely.
