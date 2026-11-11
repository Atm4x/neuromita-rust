# Same-branch Regenerate durable admission (B77)

`CharacterContextReset` remains the native **History Reset**. `MitaView(at revision)` folds historical observation, Forget and Reveal without rewriting source events. `MessageHidden` is presentation, not forgetting. Stories never reveal referenced facts by themselves.

## Atomic owner boundary

The B77 owner-only `admit_same_branch_regenerate` requires an exact, fresh `RegenerateKnowledgePlan`, a new RequestId plus captured `RegenerateConfigurationReference`, and distinct operation IDs. It rejects all unreconciled state/memory/game effects. In one `append_guarded` and one SQLite transaction, it writes `MessageHidden`, optional `CharacterKnowledgeForgotten`, and a terminal-positioned `RegenerateAdmitted` receipt; no branch is forked and the selected branch stays unchanged. On storage/CAS failure none of these facts persists. The marker records canonical accepted reply RequestId/EventId, original player EventId, generation and source head, target IDs, and captured configuration identifiers. Validation checks source causality, marker/batch equality, uniqueness within the visible branch, private operations and original input.

The receipt proves **admission** only: it is not proof of dispatch or acceptance and must not be used to requeue after crash. The legacy fork lifecycle (`BranchCreated`, cancellation, dispatch, recovery) is **not** yet migrated to this schema. Hence this owner method is deliberately unconnected to `ChatApiState` and must not be exposed as a public Regenerate operation. The caller must reserve a queue permit and freeze all request settings first; a later integration slice will connect request registration, cancellation and completion to this immutable marker.

Remaining: session-wide request uniqueness across divergent branches; archive tamper test, explicit new-marker recovery, acceptance and cancellation CAS, old effect invalidation and semantic-memory leakage audit, preserving other characters. No Unity effects are rolled back by hiding or Forget.
