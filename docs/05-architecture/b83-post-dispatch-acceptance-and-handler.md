# B83 — Same-branch Regenerate after durable provider dispatch

B82 proved that a foreign SQLite owner's knowledge mutation between durable admission and provider dispatch prevents the external call. B83 covers the next window: the provider dispatch marker already exists, and a second SQLite owner commits a Forget, Reveal, or History Reset before the response is accepted.

## Durable acceptance boundary

A request that has committed `RegenerateMayHaveDispatched` is not permission to persist an answer against a stale knowledge epoch. A real `CharacterHandle` validates the captured `TimelineTurn` and the current cause, then `append_reply_tracked` commits message and state effects together. The independent SQLite writer advances the branch while the provider is in flight; an obsolete acceptance is rejected without applying `CharacterStateChanged`. The initial player `EventId` remains the same, no branch is forked, and historical `knows_event_at(..., through)` views are immutable. The foreign operation is an explicit `CharacterKnowledgeForgotten`, `CharacterKnowledgeRevealed`, or `CharacterContextReset`; an ordinary story never implies Reveal.

Three backend integration tests demonstrate this using `CharacterHandle::accept` after the dispatch marker. Four additional tests execute the full `MessageHandler::run_queue` path with a controllable `GenerationProvider` and the actual character actor. The provider is held after the durable marker while an independent SQLite owner changes the knowledge projection, then released. The normal path accepts a response with the requested state effect exactly once; the three foreign mutation paths return a non-success outcome, preserve the character state, and leave a durable abandoned receipt. Each test verifies that one provider call was made, with no automatic replay.

## Known limits

The controlled provider is a test provider, not a live external LLM or Unity effects sink. These tests establish **no durable acceptance or effects after a foreign knowledge change**, not erasure of knowledge already sent to a provider before that change. A remote provider can have received a prompt and performed irreversible external work. The in-memory handler cannot revoke that historical disclosure. Other unclosed gates include post-commit volatile registration crash gaps, UI activation, Unity physical effect reconciliation, semantic and summary in-flight cache policies, and remote exactly-once. Keep public Regenerate disabled.
