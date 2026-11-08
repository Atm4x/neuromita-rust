# B69 — truthful foreign-acceptance terminal evidence (backend-only)

A canonical Timeline `AcceptedReply(EventId)` belongs to a regeneration admission
(`RequestId` + source `ReplyTo` + privacy proofs) and can be committed by a
separate SQLite owner. It **does not** prove the complete `AcceptedResponse`
DTO: that requires segments, synthesized audio, `state_after`, context anchor,
possible game invite and their ownership/validity. B69 never fabricates that DTO.

When `RegenerateCancellation::inspect_regenerate` observes the durable accepted
reply, `RequestRecord` seals an independent **typed terminal resolution**:
`RequestSnapshot.durable_resolution = AcceptedElsewhere(EventId)`. Watchers
are notified immediately; `RequestHandle.wait()` and `RequestObserver.next()`
return `ObserveError::DurablyAcceptedElsewhere(EventId)` rather than hanging.
The last volatile `RequestState` remains the true local processing stage, not a
forged `Finished(Success)` nor `InternalError`. `RequestSnapshot::is_resolved()`
is the authoritative composite completion check; existing UI `RequestState`
rendering is unchanged until a separate UI migration is authorized.

The record is retired from active discovery and stored in diagnostics with
`AcceptedElsewhere {event_id}` as a *non-error* outcome. No provider replay,
additional reply, effect, asset allocation, abandonment or branch switch occurs.
The processing token is cancelled only **after** durable acceptance was read,
preventing further local work from modifying a resolved request.

Limits: This is a typed observation resolution, not a fully reconstructed
`AcceptedResponse` and not a claim of exactly-once external HTTP execution.
`RequestState::is_terminal()` alone does not include this separate resolution;
backend request monitoring must use `RequestSnapshot::is_resolved()`. Clients
that require an accepted rich result should inspect the canonical Timeline
by their own privacy-authorized APIs. Public Regenerate remains disabled.
