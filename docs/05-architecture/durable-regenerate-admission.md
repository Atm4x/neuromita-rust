# B62 — immutable Regenerate admission identity in historical fork marker

B61 queued a Regenerate after committing its selected historical fork, leaving a crash window with no durable request identity. B62 moves creation of the new `RequestId` **before** owner admission and serializes a typed `RegenerateAdmissionReceipt` in the child `BranchCreated` marker; the marker, child branch, active-selection CAS and revision are persisted in one native SQLite `BEGIN IMMEDIATE` transaction. A failed preflight, SQL write, or selection update leaves no child and no admitted receipt. There is no separate request table or independently committed admission fact.

## Proven envelope

- The immutable marker carries the new request, canonical old accepted request/message, original player EventId, character, parent branch/head/generation, child branch, and a stable **configuration reference** (settings revision, preset identifier, provider model and prompt set). It never copies source text/media or provider credentials. The player input still exists only in the canonical parent prefix; the old accepted reply is excluded in the child branch.
- `TimelineStore::load_persisted` validates each durable marker against the original parent's canonical accepted-reply receipt, a single `ReplyTo` edge, addressed player input, and anchor sequence. It rejects forged sources and duplicate durable RequestIds. The same validation runs on SQLite reopen and archive import. The native SessionArchive retains the typed marker rather than inventing another source of truth.
- Queue permit, settings, preset, prompt capture and owner acceptance are ordered as in B61. The newly generated ID is fixed before the fork and is the ID registered into volatile `RequestRegistry`, then sent via the reserved permit. Cancellation after acceptance does not erase the committed branch or receipt.
- Reading the marker after a crash proves *admission identity*, not that the volatile queue accepted, dispatched, ran, cancelled or completed that command. No unsafe automatic replay is performed.

## Remaining limitations

- The configuration reference is not a replayable serialized provider/prompt snapshot. Independent preset edits and prompt changes must be reconciled before any durable recovery/re-enqueue policy; attempting to regenerate with live settings would violate frozen-configuration semantics.
- A process can still crash between SQLite COMMIT and volatile registry registration/queue dispatch. In B62 this no longer loses request identity, but no durable `dispatched`, `cancelled`, `accepted`, `recovery-abandoned` lifecycle ledger or idempotent retry API exists. Recovery must not blindly requeue unresolved receipts and must not claim exactly-once external provider invocation.
- Late provider callback and effect/asset commit atomicity across selection switches/cancel still require integration proofs. Do not expose accepted Regenerate in ChatApi or UI.
- External Python EventObserved receipt/observer/identity, media GC roots and physical capture are outside this owner slice.
