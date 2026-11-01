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

## Read-only request evidence

`ApplicationLifecycle::request_evidence_at(character, request_id, anchor)` provides
**canonical evidence**, not a restored live request state. It inspects one
immutable branch prefix, including fork cutoffs, without switching selection:

- `AcceptedReply(fact)`: an accepted `CharacterMessage` with that `RequestId`
  and actor was committed in the prefix. Does not prove external effects succeeded.
- `PlayerInput(fact)`: an admitted `PlayerMessage` carrying that `RequestId`
  and explicitly visible to the character exists, **but no accepted reply**
  exists in the prefix. This does not prove generation started or remains pending.
- `ProactiveTrigger(fact)`: an addressed `ProactiveOpportunity` was recorded
  without an accepted reply. For react/initiate this means preparation reached
  the timeline, **not** that the request completed or that LLM started.
- `NoRecord`: no matching canonical fact. This is not proof of cancellation,
  failure or even that admission did not happen (e.g. react queued but never run).

Ambiguous duplicate evidence of the same kind, or both an admitted player input
and proactive trigger for one request/character, returns `Conflict` rather than
selecting an arbitrary event. A message-only `ChatApi::publish_message` has no
`RequestId` and is not misidentified as admission. Cross-character facts and
facts beyond the requested historical cutoff are not considered.

This requires **no extra durable status table**. It cannot recover in-memory
`RequestHandle`, reproduce voice output, guarantee exactly-once external effects,
or recover a `RequestId` lost when the process crashes between storing its
input and returning the handle. Full durable request lifecycle remains open.
