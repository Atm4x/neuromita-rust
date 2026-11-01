# One canonical conversation message

**Normative knowledge rule:** [KNOWLEDGE.md](KNOWLEDGE.md). Hearing a retelling
of event X creates a new message event Y; the listener learns Y, not X.
The older late-disclosure behavior below is an implementation gap, not an
approved semantic contract.

A conversation utterance is one `ConversationMessage`, regardless of its producer:
player, character, or an accepted generated reply. Its `speaker` is a typed
`ConversationSpeaker::Player | Character(CharacterId)`; the Timeline event kind
(`PlayerMessage` or `CharacterMessage`) is derived from that identity, never
chosen by a separate writer. Both explicit publication and accepted replies
use `ConversationMessage::into_draft` to produce the same canonical fields.

`ConversationAudience` carries three separate concepts:

- `targets: BTreeSet<ConversationSpeaker>`: the intended recipients, including
  the player or multiple characters. Targets are metadata, **not** knowledge.
- `participants: BTreeSet<ConversationSpeaker>`: the semantic dialogue roster.
  Merely participating does not imply hearing a particular utterance.
- `seen_by: BTreeSet<CharacterId>`: the characters who actually acquired the
  utterance. This alone determines knowledge for the MitaView. A character
  speaker is always included in its own witnesses, even when omitted by the
  caller. Player knowledge is not modeled as a separate MitaView.

The canonical fact has a single EventId, sequence, author, JSON content and
optional causal links. Per-character views derive `assistant` for the author
and `user` for other listeners from that same fact. A character can speak to
another character, several characters, or the player; the audience does not
need to contain the player. Only characters that directly observed this
utterance may know this event. If a character tells someone about an earlier
utterance, that retelling is **another** `ConversationMessage`; it must not
retroactively disclose the original. The former `disclose_conversation_message` / `EventObserved` mechanism has been removed; retellings must be new messages.

## Producers

- `ApplicationLifecycle::publish_conversation_message` publishes a complete
  `ConversationMessage` without scheduling a generator.
- `ChatApi::publish_message` is a compatibility convenience for private player
  input; it also builds a `ConversationMessage` with the same conversion.
- `ChatApi::reply` admits its player message through that conversion **before**
  generation and records its generated `CharacterMessage` using the same
  conversion. `ReplyRequest::with_input_audience` controls who heard the player
  input, while `with_audience` independently controls the generated answer.
  They are two distinct utterances, not two Timeline storage mechanisms.
- `ReactRequest::with_audience` and `InitiateRequest::with_audience` control
  the generated answer. Their instructions are `ProactiveOpportunity` control
  events, not invented player messages.
- Without explicit audience configuration, existing private one-character
  behavior is preserved. A reply cannot be admitted if its requested character
  is absent from the input's `seen_by`.

Generated replies still pass the existing selection/context-epoch/revision
checks in the same guarded append. All fields required by the legacy
`CharacterReply` read model remain attached to the one canonical fact.

## Current boundaries

This is **not** automatic broadcast. The sender's dialogue roster and real
witnesses must be supplied by the caller. Unity's live dialogue participant
rules and network IPC are not implemented here. `React` can currently respond
to a known character message, but `ReplyRequest` does not yet directly bind its
trigger to an already-existing character message: ordinary `reply` still
admits a new player utterance. That dedicated existing-message reply workflow
remains open in Issue #10. Full Python UI/provider time/metadata parity is
tracked in Issue #2. Crash recovery is explicitly deferred in Issue #54.
