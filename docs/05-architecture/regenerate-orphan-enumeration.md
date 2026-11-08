# B70 — read-only enumeration of durable Regenerate admissions

## Contract

`SessionTimeline::enumerate_regenerate_admissions(character)` discovers durable fork markers only within the owner's session, filters by the requested character, validates the stored admission against its canonical parent and player input, rejects repeated `RequestId`s, and returns typed `RegenerateRecoverySnapshot`s. Outcomes distinguish `DispatchUnknown`, `MayHaveDispatched`, `AcceptedReply`, and `Abandoned`. **Neither unknown outcome grants a provider retry.** Enumeration does not change selection, request state, Timeline events, or trigger inference.

`TimelineRepository::regenerate_admission_branches(session, character)` is the discovery boundary. In SQLite it uses an indexed session-child-branch lookup (`timeline_session_child_branches`) joined to exactly one `BranchCreated` event (`timeline_events` composite primary key); it does not scan unrelated Timeline facts or sessions. The transient implementation uses in-memory branch records. Each discovered branch is independently inspected using the native read-only recovery path, including accepted reply causality, abandonment and potential dispatch evidence.

A two-part fence (`timeline_revision` plus the persisted `timeline_active_selection` generation) surrounds the enumeration. Ordinary branch selection updates generation without necessarily advancing the Timeline fact revision, so checking revision alone would miss A→B→A changes. If either fence changes during collection, the owner returns `ConcurrentWrite` rather than a mixed result. This is a read-side consistency check, not a separate durable registry or global lock. Caller access remains internal; character identity must originate from the trusted request owner. No cross-character payload is returned.

## Coverage and limitations

- Historical children remain discoverable when the active selection moves A→B→A. Their result records the selection generation at inspection time.
- SQLite reopen and native archive import preserve the durable fork markers; candidate discovery can operate on imported `TimelineStore` without invoking providers.
- Enumeration currently returns a whole-session vector and classifies each candidate by reading its branch world. Indexed **discovery** scales with session branches, while classification scales with the corresponding branch histories; a paginated/indexed per-request lifecycle projection would be a separate optimization.
- A `MayHaveDispatched` marker only proves a pre-dispatch authorization, not a network call; `DispatchUnknown` is not proof of non-dispatch. No automatic dispatch/replay, volatile request recreation, or reconstruction of `AcceptedResponse` fields is performed.
- The owner does not expose this as a public UI or remote API; permission checks at those entry points must be designed before exposure. Provider HTTP exactly-once remains outside Timeline's guarantees.
