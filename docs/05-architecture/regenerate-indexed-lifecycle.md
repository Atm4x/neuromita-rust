# B71 — indexed Regenerate lifecycle evidence (backend only)

`SessionTimeline::enumerate_regenerate_admissions(character)` retains the B70
session-and-recipient-scoped discovery and the same `timeline_revision` plus
persistent `timeline_active_selection.generation` read fence. Discovery neither
modifies Timeline nor grants a provider retry. `DispatchUnknown` never means
"safe to dispatch".

For every validated immutable admission marker, `reconcile_regenerate` now uses
`TimelineRepository::regenerate_request_facts(child, request)` rather than
materializing the entire child world. Facts are restricted to the admitted
child branch and the immutable request ID. The repository contract does not
return another character's events or events in a parent/sibling branch.
SQLite implements the lookup through an expression index on the authoritative
`timeline_events.fact_json` payload, ordered by branch-local sequence. There is
no second durable lifecycle table or independent state projection. The transient
implementation applies the same identity filter to its in-memory world.

Canonical accepted-source validation previously performed a full parent-world
projection through the default `accepted_reply_at`. SQLite now uses the same
request index across the bounded ancestor chain (including each fork cutoff and
an optional historical `through` bound). It applies the shared
`domain::accepted_reply_request` predicate and rejects multiple canonical
replies; this keeps recipient and reply classification in the domain. The
underlying immutable facts remain the only source of truth.

## Proof boundaries

- Branch/session/recipient filtering still occurs before exposing a snapshot.
  The canonical receipt is revalidated before lifecycle classification.
- A local lifecycle lookup is ordered, read-only and request-scoped. It cannot
  create an `AcceptedResponse`, reapply an effect or initiate provider traffic.
- SQLite's query planner is tested to select the new expression index, including
  after database reopen. Historical selection A→B→A is still fenced by both
  revision and persisted generation at the enumeration boundary.
- Indexed reads execute inside native SQLite read transactions. A competing
  committed change during enumeration yields `ConcurrentWrite` via B70 fences.
- Complexity of the lifecycle evidence query is proportional to the number of
  matching request facts, not the length of the admitted child's world; source
  validation visits only the bounded lineage, not all events on each ancestor.
- This is **not** paginated discovery. Enumeration still allocates the full
  character-scoped admission list, and B70's marker discovery reads all child
  fork markers in that session. The B71 indexes remove costly per-branch world
  reads but do not claim bounded-memory pagination or exactly-once external
  provider calls.

No UI, Slint, Presenter, legacy code or Unity/dotnet build is involved.
