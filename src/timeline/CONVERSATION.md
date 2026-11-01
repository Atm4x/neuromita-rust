# Shared conversation facts

The canonical Timeline stores one `PlayerMessage` or `CharacterMessage` for a
conversation utterance, not one copy per character. The public
`ApplicationLifecycle::publish_conversation_message` accepts a stable event ID,
external message ID, typed speaker, content, target, dialogue participants,
actual witnesses and optional causal links. It does **not** initiate generation.

- `participants` is semantic dialogue metadata and is not proof of observation.
- `witnesses` is the knowledge audience. A character speaker always knows its
  own message. Each character's `MitaView` derives its own `assistant` / `user`
  role from the speaker, retaining the same canonical EventId and sequence.
- `ApplicationLifecycle::disclose_conversation_message` adds an `EventObserved`
  fact when another character learns an existing utterance later; it never
  duplicates or rewrites the original message. The learned time is the
  disclosure's sequence, rather than the original utterance's sequence.
- Replay of a message or disclosure with unchanged identity and content is
  idempotent. Conflicting identity reuse fails. The original witness set is
  immutable; later audience changes are disclosures, not message replacements.
- Reset only changes the recipient's current knowledge. Historical snapshots,
  other listeners' views and the canonical utterance remain intact.

The transport-specific dialogue roster, `can_hear_speaker` interpretation,
identity mapping from Unity actors and real LLM reply fan-out are **not**
implemented by these functions. Existing single-character `ChatApi.reply` remains
unchanged. Provider/UI timestamp and complete Python metadata parity remain in
Issue #2; full conversation turn integration remains in Issue #10.
