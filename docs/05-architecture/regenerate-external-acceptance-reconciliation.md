# B68 — reconcile accepted Regenerate against a late local error

The canonical authority is the selected child Timeline, not a `RequestRecord` outcome. A second SQLite owner can commit `AcceptedReply(RequestId)` and its effects before the original process observes a provider error, cancellation or shutdown. Appending `RegenerateAbandoned` must then fail; this is a terminal conflict, **not** permission to publish `InternalError`, `Cancelled`, or a fabricated `AcceptedResponse`.

## Contract

- `RegenerateCancellation::inspect_regenerate(child, request)` returns the native read-only `RegenerateRecoverySnapshot` and validates the child admission receipt. An operator can inspect an orphan even without any volatile request record through `ChatApiState::inspect_orphaned_regenerate`. Neither operation submits work or replays the provider.
- After a failed abandonment, `RequestRecord` asks the owner for the canonical outcome **without holding its own mutex**. If it observes `AcceptedReply(event)`, it records a request-local `PendingTerminal::AcceptedElsewhere(event)`. This is a cache of verified evidence, not another durable source of truth. It forbids a later fallback to the failed provider outcome.
- The explicit registry reconciliation returns `RegenerateTerminalReconciliation::AcceptedElsewhere(event)` rather than a generic successful abandonment. Repeated reconciliation is idempotent. A true successful abandonment returns `Abandoned` and completes the original volatile failure/cancellation normally.
- When externally accepted, the original volatile request deliberately **remains nonterminal and registered**. Its rich `AcceptedResponse` was not reconstructed and must not be invented. This is a remaining integration blocker for request observers and public Regenerate enablement; a future dedicated terminal projection is required, with corresponding consumers and tests.
- `Conflict`, `ConcurrentWrite`, and storage failures that are not proven accepted remain unresolved. Cancellation returns the existing conservative `TooLate` instead of introducing UI-visible enum variants.

## Proof boundary

B68 tests use a second SQLite owner to accept a reply with a `CharacterStateChanged` effect. A late error, explicit cancel, shutdown, or retry after an injected SQL abort cannot create an abandonment, erase effects or duplicate acceptance. A read-only orphan check survives A→B→A, SQLite reopen, and SessionArchive export/import with one original reply and one effect. Existing B64 two-writer acceptance-versus-abandonment CAS tests remain active.

This does **not** prove exactly-once external HTTP, automatic restart/requeue, completion of an already-accepted rich response DTO, or end-to-end public Regenerate. No UI/Slint/Presenter changes are made.
