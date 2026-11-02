# Long-term memory lifecycle (Core)

The authoritative data structure is `CharacterMemories`, captured and replayed by `SessionTimeline`. Knowledge retrieval receives only that captured projection; no new memory store, model or worker is introduced.

- Legacy JSON records (id, priority, content) decode as `kind=Fact`, `state=Active`, `expires_at_unix_ms=None`. Default values are omitted when writing them back. Legacy `MemoryChange::Delete` still means physical removal; existing Timeline events retain their original semantics.
- `MemoryState`: `Active`, `Forgotten`, `Deleted`. Ordinary prompt facts are active only. Forgotten facts are retained separately for future *explicit* recall; deleted records remain for inspection and explicit restore/purge, never prompt selection. No implicit restore occurs on a duplicate `Add`.
- `MemoryKind`: `Fact` or one of five island categories: `relationship`, `opinion`, `preferences`, `commitments_conflicts`, `language`. A character has at most one non-deleted record of each island category; `SetIsland` updates the stable ID of an active island. Islands are separate from the active-fact budget and prompt block. They are not embedded.
- Capacity: at most 256 active facts and 1,024 total retained records (including forgotten/deleted/islands). No eviction is performed as a write-side surprise. Exceeding a bound rejects the proposed write transaction.
- TTL is represented by an explicit expiration cutoff. `resolve_expirations(as_of_unix_ms)` deterministically proposes `Upsert(Forgotten)` changes; it does not commit anything. Callers must apply those changes through the existing guarded Timeline append. Reads never change history. No background TTL scheduler is claimed.
- `Forget`, `Restore`, `SoftDelete`, `SetExpiry`, `SetIsland` are explicit domain commands, distinct from the legacy LLM `memory_delete` operation. Updating existing IDs cannot change their kind. Purging corresponds to the legacy physical `Delete` operation; UI confirmation/backup workflows remain future scope.
- The request orchestration projects active islands into a bounded `[MEMORY ISLANDS]` data block and ranks ordinary active facts separately. The inference boundary remains `EmbeddingProvider` implemented outside the domain; island projection never invokes inference.

**Not yet complete:** user-facing editor, scheduler/maintenance worker, protected rules, timestamps and usage-frequency policy, forgotten-memory recall and backup-before-purge. These require explicit behavioral decisions before wiring automated changes into Timeline.
