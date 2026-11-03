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
| Capacity | `_forget_over_limit_memories` uses configurable `MEMORY_CAPACITY` (default 50), forgets noncritical active facts in priority/age order; optionally uses `access_count` | Hard bounds remain 256 active facts and 1024 retained records; `CharactersPresenter` additionally offers explicit preview/confirm for forgetting by Low→Normal→High, then stable ID | **Not equivalent**. No automatic eviction. Rust lacks Python `date_created` and `access_count` ordering; stable ID is not chronology. |
| TTL | `apply_ttl_cleanup` uses `MEMORY_TTL_ENABLED`, priority-specific days and `date_created`/`last_accessed`/`access_weighted` modes | Explicit `expires_at_unix_ms` per record plus startup/periodic guarded maintenance | **Not equivalent**. Python metadata, access semantics and settings contract require explicit design |
| Dedup | `add_memory` optionally uses lexical Jaccard `_find_duplicate_memory`; `skip_if_exists=True` uses **exact** SQL content equality and reactivates a matching forgotten/deleted row | Normal `Add` keeps normalized exact dedup; explicit `EnsureFact` matches exact fact text, reactivates a retained forgotten/deleted ID and assigns the requested priority; already-active matches are no-ops | `EnsureFact` is covered by deterministic Rust fixtures and Presenter/SQLite tests. Ambiguous retained matches fail instead of depending on SQLite row order; normalized-collision inserts remain forbidden. Optional Python lexical Jaccard dedup, settings and current Python runtime parity remain open. |
| Memory observation | Python owns mutable DB/UI state | `CharactersPresenter::observe_memories` observes **this Core's** committed memory changes and reads an authoritative snapshot | External SQLite writers do not trigger the local watch; cross-process IPC is a separate task |

Completed guarantees: canonical identity, character isolation, states, soft-delete,
protected records, bounded read projections, guarded atomic user edits, read-only
forgotten recall, safe backup-first purge and local Presenter observation.

Still required to fully close #21: confirm which Python paths are actually
used in the latest product, choose the differing capacity/TTL/dedup policies,
cover those choices with golden behavior tests, and agree on the administrative
authorization boundary. This report does **not** declare #21 closed.

## Opt-in exact idempotent insertion (implemented)

`MemoryCommand::EnsureFact` corresponds to the **fact-only** intent of Python
`add_memory(..., skip_if_exists=True)`: exact (case-sensitive) text equality;
`Active` => no operation, `Forgotten`/`Deleted` => same immutable ID restored to
`Active` with the supplied priority, and no matching retained fact => insert.
This command does **not** switch ordinary `Add` to automatic restore or enable
AI dedup. A protected hidden match, multiple retained exact matches or a
normalized-content collision fails atomically. It keeps explicit expiry;
reactivation does not silently reset TTL metadata. If the prior expiry is
already elapsed, the restored state is not eligible for ordinary generation
until the owner explicitly updates its expiry.

Python selects `LIMIT 1` without deterministic ordering and does not
restrict that SQL query to facts. Rust deliberately rejects ambiguous matches
and does not allow a fact operation to overwrite a typed island. These are
recorded safety differences, **not** full behavioral parity claims.
