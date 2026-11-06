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
| Memory islands | `upsert_island` updates the sole active island; DSL `SEED_ISLAND` uses `seed_island`, which refuses overwriting *any* retained island row (including deleted) | `SetIsland` updates an active island; explicit `SeedIsland` is a one-time insertion and preserves forgotten/deleted history | Domain and Presenter fixtures cover one-time seeding, isolation, no-op notifications and protection. Repair/migration of duplicate legacy Python islands and current-runtime parity remain open. |
| Capacity | `_forget_over_limit_memories` uses configurable `MEMORY_CAPACITY` (default 50), forgets noncritical active facts in priority/age order; optionally uses `access_count` | Hard bounds remain 256 active facts and 1024 retained records; `CharactersPresenter` additionally offers explicit preview/confirm for forgetting by Low→Normal→High, then stable ID | **Not equivalent**. No automatic eviction. Rust lacks Python `date_created` and `access_count` ordering; stable ID is not chronology. |
| TTL | `apply_ttl_cleanup` uses `MEMORY_TTL_ENABLED`, priority-specific days and `date_created`/`last_accessed`/`access_weighted` modes | Explicit `expires_at_unix_ms` per record plus startup/periodic guarded maintenance | **Not equivalent**. Python metadata, access semantics and settings contract require explicit design |
| Dedup | `add_memory` optionally uses lexical Jaccard `_find_duplicate_memory`; `skip_if_exists=True` uses **exact** SQL content equality and reactivates a matching forgotten/deleted row | Normal `Add` keeps normalized exact dedup; explicit `EnsureFact` matches exact fact text, reactivates a retained forgotten/deleted ID and assigns the requested priority; already-active matches are no-ops | `EnsureFact` is covered by deterministic Rust fixtures and Presenter/SQLite tests. Ambiguous retained matches fail instead of depending on SQLite row order; normalized-collision inserts remain forbidden. Optional Python lexical Jaccard dedup now has an explicit Rust `AddLexicallyDeduplicated` command with threshold in per-mille (500..1000), Russian light stemming, an active-fact-only candidate set, stable `MemoryId` tie-breaking and atomic priority/text updates. Ordinary `Add` is unchanged. Python still enables lexical dedup automatically by default and uses row iteration for equal similarity, so automatic policy/settings and current Python runtime parity remain open. |
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

## Initial character-island seeding (implemented)

Python `src/DSL/dsl_engine.py` uses `MemoryManager.seed_island` for the
`SEED_ISLAND` instruction. Its check includes deleted rows and does not update
existing islands. Rust `MemoryCommand::SeedIsland` implements that **explicit**
one-time intent: inserts a `High`-priority island if there is no retained
island of the specified category, otherwise emits no change. It does not
reactivate `Forgotten`/`Deleted` islands or overwrite user-modified/protected
records. `SetIsland` remains the separate upsert operation, preserving its
previous semantics. No background AI, automatic TTL reset or GUI is involved.

## Explicit lexical fact deduplication (implemented)

`MemoryCommand::AddLexicallyDeduplicated { priority, content,
threshold_per_mille }` is an opt-in command for a purely deterministic lexical
Jaccard path. It uses the Python 2026-10-22 `memory_manager.py` tokenization and
`utils/ru_stem.py` light Russian stemmer (Latin letters, digits and Cyrillic,
tokens shorter than three characters ignored, `ё` normalized in stemming).
Default-equivalent threshold is `800` for 0.8; explicit valid range is
`500..=1000`, rejected outside that range instead of silently clamped.

Only active ordinary facts are candidates, not forgotten/deleted facts,
islands, summaries or embeddings. The highest Jaccard hit wins; for exact
similarity ties the smallest `MemoryId` wins rather than relying on SQLite's
unspecified row ordering. A match preserves the canonical ID, rewrites the
content and upgrades priority to the maximum. Protected matches are rejected
rather than edited. Missing matches use the existing Add contract. The whole
batch is resolved in the character domain before the guarded Timeline append.
The ordinary `Add` and `EnsureFact` commands remain unchanged; no automatic
inference, semantic similarity or background dedup has been introduced.

Remaining parity limits: Python may run this path automatically depending on
`MEMORY_DEDUP_ENABLED`, while Rust requires explicit intent. Python updates
creation/usage metadata when rewriting an existing row; those fields do not
exist in the Rust canonical `MemoryRecord`, so timestamp, age and access-based
forget policies remain separate architectural decisions. Python may produce
different results for equal-ranked SQL rows; Rust is intentionally deterministic.

## Active structured-response insertion and corrupt-data recovery (2026-11-07)

The read-only Python reference at SHA
`5f3d061f523d9abefa6a264bb2953048a935fa65` calls
`MemoryManager.add_memory(priority=..., content=..., entities=...)` from
`src/characters/character.py::_apply_structured_memory_ops` for each
`StructuredResponse.memory_add`. That call leaves `skip_if_exists=False`;
`src/managers/memory_manager.py::add_memory` therefore attempts lexical dedup
when `MEMORY_DEDUP_ENABLED` (default `True`) and the entry is an ordinary
fact. The threshold uses `MEMORY_DEDUP_THRESHOLD` (default `0.8`). The same
method also supports the separate exact-text `skip_if_exists=True` path.

Rust currently reaches `MemoryCommand::Add` from the local GUI memory editor
(`src/bin/neuromita-gui/memory.rs`), while initial prompt seeding uses
`SeedFromPrompt`. No production structured-response memory-add path selecting
`AddLexicallyDeduplicated` was established in this audit. Therefore the
Python setting must **not** be represented as already effective in Rust, and
changing ordinary `Add` globally would alter explicit user edits. Integrating
model-initiated writes requires coordination with the Generation/accepted-turn
owner; this audit does not wire that path.

The SQLite failure-contract tests
`malformed_persisted_memory_blocks_new_memory_writes_until_repaired` and
`invalid_second_memory_change_rolls_back_entire_batch_and_allows_retry`
exercise corrupt-data admission and partial-write recovery. A syntactically
valid but semantically invalid persisted `MemoryChange` is **not** interpreted
as missing memory or silently skipped: further memory appends for the affected
world fail until the original fact is repaired through an external, controlled
recovery path. A batch whose later memory effect is invalid leaves no new
events, head advance or operation receipt; a corrected retry with the same
operation key succeeds. This proves fail-closed behavior for these fixtures,
**not** general automatic database repair, damaged-page recovery or full
Python parity. No storage schema or canonical Timeline contract changed.

## Request-scoped model memory insertion policy (2026-11-07)

`generation::ModelMemoryAddPolicy` is a typed, immutable *conversion intent* for
accepted structured model responses: `Ordinary` keeps the pre-existing
`MemoryCommand::Add` behavior (normalized exact-content idempotency), while
`Lexical { threshold_per_mille }` converts **only** `memory_add` into
`MemoryCommand::AddLexicallyDeduplicated`. Every `memory_update`, delete, merge,
state delta, reminder and user-initiated memory edit retains its existing
semantics. Invalid per-mille thresholds (`<500` or `>1000`) reject the entire
candidate before Timeline acceptance. The original `from_structured` entry
point stays unchanged; the request owner may explicitly call
`GenerationRequest::with_model_memory_policy` with policy captured at
request creation; `ResponseGenerator` carries that value through the provider
and post-DSL phases into `ProposedResponse::from_structured_with_memory_policy`. Invalid
policy is rejected before provider invocation, not after inference.
Intermediate tool responses do not execute memory writes. Neither conversion writes memory or opens a second database.

This defines the precise adapter point for Python's settings-gated dedup path.
The active Python reference enables lexical similarity by default under
`MEMORY_DEDUP_ENABLED`; Rust now captures its own validated, explicitly
selected policy through `AppSettings.memory` and `MessageHandler` before
inference. The application default deliberately remains `Ordinary` so
pre-existing saved fact semantics do not change silently.

## Application setting to generation request (2026-11-07)

`settings::MemorySettings` adds an explicit, validated application setting for
**model-originated** `StructuredResponse.memory_add`. The existing `SettingsApi`
`SettingsSection` read/edit/replace and JSON storage paths operate on the same
canonical `AppSettings.memory` field; no separate settings owner or versioned
schema was introduced. A stored Rust settings document without the `memory`
field loads the current default. Invalid `threshold_per_mille` values outside
`500..=1000` fail settings validation rather than being clamped.

`request::MessageHandler` reads the already captured `SettingsSnapshot` and
passes `settings.values.memory.model_add_policy()` into the generated request.
The resolved value remains immutable while provider inference runs and does
not affect manual memory commands, user-facing editor writes, or other proposed
model effects. The initial Rust setting is deliberately **disabled** with a
stored threshold of `800`: this preserves the previously accepted Rust
`Ordinary` behavior and avoids merging unrelated but highly similar entries.
Enabling the setting explicitly obtains the Python-style lexical behavior;
this is not a claim of identical Python defaults. The existing direct
`GenerationRequest::new` constructor still uses `Ordinary` unless the caller
supplies the policy.

Verification: `settings_transfer` covers serialization, absent-field defaults,
invalid values and reconstruction; the `vertical_slice` model-memory scenario
runs live generated responses through the fake Gemini endpoint and verifies
both the enabled dedup mode and disabled ordinary Add behavior. This setting is also exposed in the existing Slint `ModelsSettingsPage`
under **Memory → Memory hygiene**. The toggle and fixed-point threshold field
are read from the canonical SettingsSnapshot via `SettingsUi`, and changes
pass through the existing `settings::execute` GUI administration path and
`SettingsPresenter.memory.state` binding. The input accepts exact `0.500`
through `1.000` values with three fractional digits, maps them to per-mille
without floating-point rounding, and rejects malformed values. The manual
memory editor and unrelated placeholder controls on the Models page remain
unaffected; the UI does not introduce another settings owner or store.
