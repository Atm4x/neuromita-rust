# B81 — post-commit accepted-elsewhere race

The durable same-branch accepted reply may be committed by `CharacterHandle.accept` while a concurrent request cancellation reconciles that fact, retiring the volatile request as `DurablyAcceptedElsewhere`. This can happen after `begin_accepting` and before `MessageHandler` calls `mark_applied`.

Previously `mark_applied` unconditionally asserted that the volatile phase was `Accepting`. The reconciliation path legitimately changes it to `Applied`, so the assertion could panic a provider worker after successful durable acceptance.

`mark_applied` now returns false for a previously sealed external durable acceptance, and `MessageHandler` stops without duplicating a completion or game follow-up action. Other invalid phase transitions still assert. A SQLite integration regression uses an actual `CharacterHandle` to prepare and accept a same-branch request with `CharacterStateChanged`, then cancels the request in the post-commit/pre-volatile-mark window and verifies terminal truth and state after reopen.

This is not a model-provider end-to-end test and does not establish remote exactly-once delivery, crash-gap registration recovery, semantic derived-cache invalidation, or physical Unity effect reconciliation. Public Regenerate remains disabled.
