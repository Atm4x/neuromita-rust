# Character memory ownership and presenter contract

- **Character domain** (`character/memory.rs`) owns `MemoryRecord`, `MemoryCommand`, `CharacterMemories`, canonical lifecycle, validation and capacity policy. `crate::memory` remains a compatibility re-export; there is no second implementation.
- **CharacterHandle** is the existing actor/owner of user-originated changes. `memories` returns a `CharacterMemorySnapshot` with character identity, all retained records, selected branch and full Timeline HEAD. Presenter `characters.memories` and `characters.edit_memories` delegate to that same owner.
- **Timeline** owns only history, versioned selection, raw world reads and guarded event append. `character::memory_access` builds the presenter snapshot, checks the character's retained records, resolves commands with the character domain, and appends against the current Timeline HEAD. Unrelated Timeline events do not invalidate memory edits; changed character-memory records and branch selection do. No writable CharacterMemory cache exists.
- **User action** `MemoryCommand::SoftDelete` marks a record `Deleted`; the legacy physical `Delete` operation is refused at this boundary. Physical purge requires a separate confirmed backup-first administrative workflow, which is intentionally not exposed via a public presenter until there is authorization. `Forgotten` stays excluded from ordinary context/embeddings.
- **Presenter** only translates typed user intents into character-owner calls and returns the new projection. Invalid commands fail without a partial commit; stale views fail instead of overwriting newer data. A no-op returns a fresh snapshot without writing an event.
- The public presenter interface is for trusted local bindings; it is **not** a remote authorization boundary. Full UI authentication, a pushed memory change stream, Python behavioral parity and physical historical erasure are still open tasks. This refactor does not change AI Engine, Generation, or SessionTimeline wire protocols.

- LLM `memory_delete` performs `SoftDelete`, and merge marks its sources `Deleted`; neither may physically purge retained records. Existing historical `MemoryChange::Delete` remains replay-compatible.

## Read-only Presenter projections (Memory Lifecycle #21)

`CharactersPresenter::list_memories(character, MemoryListQuery)` captures **one**
character-owned snapshot and projects a read-only, bounded page (1–100 records).
Filters are optional lifecycle state, memory kind and case-insensitive substring;
ordering is the canonical stable `MemoryId` order. `total_matches` counts filtered
records while `MemorySummary` counts **all** retained records in that snapshot
(active/forgotten/deleted and fact/island/protected totals). Filtering never
changes the set available to generation or calls any embedding provider.

The page carries its complete opaque `CharacterMemorySnapshot` for a later
`edit_memories` call. `MemoryListPage::from_snapshot` can page the **same**
snapshot repeatedly without re-reading concurrent updates. The client must
explicitly refresh to observe newer memory, and edits using a stale token are
rejected by the Character owner. These types do not depend on Slint, IPC or a
particular UI toolkit. There is no separate memory cache or write endpoint.
