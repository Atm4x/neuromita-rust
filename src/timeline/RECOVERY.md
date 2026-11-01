# Reconciliation of accepted replies after restart

This is a **read-only confirmation** of a committed character reply; it is not automatic request replay or a complete crash-recovery protocol.

`ApplicationLifecycle::accepted_reply_at(character, request_id, anchor)` reads the canonical `CharacterMessage` corresponding to the supplied `RequestId` and actor in the specified immutable branch prefix. It also supports inspection of a saved branch within the same session, without switching the active selection. Only a committed character message with a valid UUID request identity is evidence of acceptance. Player ingress and proactive opportunities are **not** evidence that text generation or effects were accepted.

- `Some(fact)` establishes that the character reply was committed in the selected prefix. It preserves the immutable event identity, text, causal links, source branch and sequence.
- `None` only means no matching committed reply is present in that prefix. It **does not** distinguish crash, cancellation, timeout, failed generation, or a request whose input was never admitted.
- Duplicate accepted responses with the same request and character within one branch prefix return `Conflict`, not an arbitrarily selected result.
- The lookup makes no model, TTS, network or external-side-effect calls; it does not recreate `RequestHandle`, `RequestState` or process-local traces.
- A reset of character knowledge does not erase committed replies. Forks inherit only their checkpoint prefix; an alternative parent future is not included.
- The repository's default implementation performs a whole-branch `world()` read and filters by requested prefix. It is correct but **not** a bounded/optimized read path.

To recover a complete execution outcome across a crash, the system would additionally need a deliberately designed durable protocol covering technical request status and external effects. Do not infer those states from absent Timeline facts or write technical request statuses into canonical semantic history by default.
