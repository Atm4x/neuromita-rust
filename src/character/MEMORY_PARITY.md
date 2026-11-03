# Character Memory #21 — Python behavior audit (unclosed parity)

Reference snapshot: `Atm4x/NeuroMita` commit
`5f3d061f523d9abefa6a264bb2953048a935fa65`,
`src/managers/memory_manager.py`. This is the exported 2026-10-22 Python
snapshot, **not proof of the current Python runtime path**. The Rust branch has
its own canonical Timeline semantics; the table below records observed
behavioral differences, not a mandate to copy Python implementation details.

| Operation | Python snapshot | Current Rust Core | Gap / decision |
|---|---|---|---|
| Per-character add/update/delete | `add_memory`, `update_memory`, `delete_memory` filter by `character_id`, and delete sets `is_deleted=1` | Character-owned `MemoryCommand`, guarded Timeline append, soft delete | Core operations implemented; exact user behavior has not been golden-tested against a live Python installation |
| Merge | `merge_memories` preserves target, soft-deletes source and transfers entities | Source transitions to `Deleted`, target updated | Entity transfer does not have an equivalent typed entity field in Rust `MemoryRecord` |
| Memory islands | `upsert_island` enforces one active island per subtype; `seed_island` does not overwrite an existing island | Five typed islands, deterministic upsert and identity | Seed and repair of duplicate legacy Python island rows need parity tests/migration policy |
| Capacity | `_forget_over_limit_memories` uses configurable `MEMORY_CAPACITY` (default 50), forgets noncritical active facts in priority/age order; optionally uses `access_count` | Rejects writes above 256 active facts or 1024 retained records; no implicit forgetting | **Not equivalent**. Do not silently add auto-eviction without choosing user-facing policy and deterministic provenance |
| TTL | `apply_ttl_cleanup` uses `MEMORY_TTL_ENABLED`, priority-specific days and `date_created`/`last_accessed`/`access_weighted` modes | Explicit `expires_at_unix_ms` per record plus startup/periodic guarded maintenance | **Not equivalent**. Python metadata, access semantics and settings contract require explicit design |
| Dedup | `add_memory` optionally uses semantic `_find_duplicate_memory`; `skip_if_exists` can reactivate a matching forgotten/deleted row | Normalized exact-match dedup and explicit `Restore` command | Semantic inference belongs behind AI Engine boundary; deterministic skip/restore semantics remain to be specified |
| Memory observation | Python owns mutable DB/UI state | `CharactersPresenter::observe_memories` observes **this Core's** committed memory changes and reads an authoritative snapshot | External SQLite writers do not trigger the local watch; cross-process IPC is a separate task |

Completed guarantees: canonical identity, character isolation, states, soft-delete,
protected records, bounded read projections, guarded atomic user edits, read-only
forgotten recall, safe backup-first purge and local Presenter observation.

Still required to fully close #21: confirm which Python paths are actually
used in the latest product, choose the differing capacity/TTL/dedup policies,
cover those choices with golden behavior tests, and agree on the administrative
authorization boundary. This report does **not** declare #21 closed.
