# B79 — Request owner integration for same-branch Regenerate

This backend-only slice integrates the B77/B78 same-branch admission with the existing bounded request queue. It is **not** a public Regenerate action.

1. Reserve a queue permit before any Timeline mutation. Freeze settings snapshot, preset and prompt-set snapshot, then mint one RequestId.
2. Under the same SessionTimeline owner lock, validate the source against the selected head/generation, calculate the knowledge plan, and append MessageHidden + optional CharacterKnowledgeForgotten + RegenerateAdmitted atomically in the current branch. SQLite uses the existing guarded transaction; failure changes neither the world nor request queue.
3. Register a RequestRecord containing the immutable configuration and a typed RegenerateProtocol::SameBranch, then enqueue exactly the admitted RequestId using the reserved permit.
4. The handler invokes the explicit same-branch pre-provider dispatch fence. The record uses explicit same-branch durable abandonment and recovery for cancellation, errors, queue drain and shutdown. Legacy fork requests retain their own explicit protocol; normal replies do not consume Regenerate dispatch permits.
5. Before acceptance, verify canonical RequestId/ReplyTo/character and the *original* selection generation from RegenerateAdmitted. Re-selecting A→B→A cannot authorize a forged fresh TimelineTurn for an old admission.

History Reset remains CharacterContextReset. Forget and Reveal are direct, revisioned knowledge-control facts, not story-based disclosure. Other characters' independent histories are not rewritten. Historical `MitaView(at)` retains the earlier knowledge state.

## Safety boundary

The internal ChatApiState::regenerate_same_branch is not exposed through UI/public endpoints. The old legacy-fork API is kept for backward compatibility only and must not be used as fallback for a same-branch intent.

Not proven in B79: full actual provider end-to-end acceptance with effects; physics/Unity side-effect reconciliation; provider exactly-once across crash; derived semantic index/summary invalidation; bounded performance for large sessions. All must be verified prior to public UI enablement.
