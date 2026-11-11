# B78 — same-branch Regenerate durable lifecycle (backend slice)

The ordinary Regenerate target is **not a global history fork**. The existing shared
Timeline records the original reply, atomic character-scoped `MessageHidden` and
`CharacterKnowledgeForgotten` controls, and a `RegenerateAdmitted` receipt.
Other characters' conversations remain part of the same world.

`CharacterContextReset` is the existing **History Reset**. The historical
`MitaView(at revision)` must continue to reconstruct prior knowledge; explicit
Forget/Reveal are separate from presentation Hide. Merely talking about an old
fact does not reveal its original `EventId`.

The native lifecycle validator now resolves two *distinct* durable receipts:
legacy child `BranchCreated` and current-branch `RegenerateAdmitted`. There is
no generic fallback. For the latter, the canonical `RequestId` is allowed to:

1. Write **one** `RegenerateMayHaveDispatched` fact before entering an external
   provider. On restart this means only *may have called*; it never grants replay.
2. Write `RegenerateAbandoned` at most once; the operation is idempotent and
   works even if the active selection has changed, using the admitted branch's
   current head for the native storage CAS.
3. Accept **one** causally linked assistant `CharacterMessage` with the exact
   original player `ReplyTo`, only after the durable dispatch fence. Effects
   are accepted in the same append batch as the answer.
4. Reject a late answer after `Abandoned` and reject `Abandoned` after an
   accepted reply. Under `BEGIN IMMEDIATE`, two SQLite writers cannot both win.
5. Reject new dispatch or acceptance after a newer History Reset / addressed
   knowledge-control epoch. This protects a stale request's captured context.

Read-only `reconcile_same_branch_regenerate` classifies `DispatchUnknown`,
`MayHaveDispatched`, `Abandoned`, or `AcceptedReply` from the Timeline itself.
Unknown is **not** permission to invoke a provider. The API does not register
requests, mutate memory or load a provider.

## Safety boundary

This is an internally tested Timeline domain and owner capability, **not** a
public/user-facing Regenerate implementation. `ChatApiState` and
`RequestRegistry` still use the legacy branch-specific cancellation/dispatch
adapter. They must be explicitly migrated with queue-permit reservation and
frozen configuration before using this new lifecycle. No automatic crash replay
or unproven provider exactly-once is implemented.

Effects from *old* answers are not magically undone; B77 refuses
unreconciled old state/game side effects. Persisted character memories,
semantic indices and derived state still require a dedicated leakage audit.
