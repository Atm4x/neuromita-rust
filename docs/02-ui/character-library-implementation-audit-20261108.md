# Character Library: Python → Rust Core → Slint audit (2026-11-08)

## Pinned source revisions

- Python production tree: [VinerX/NeuroMita@`36177e5a559e86d757a0f7bc78c8ef03943eb681`](https://github.com/VinerX/NeuroMita/tree/36177e5a559e86d757a0f7bc78c8ef03943eb681).
- Rust upstream before this work: [Atm4x/neuromita-rust@`3433487564e41a2cce582d060e3329b91afb698e`](https://github.com/Atm4x/neuromita-rust/commit/3433487564e41a2cce582d060e3329b91afb698e).
- Core implementation dependency: [PR #309](https://github.com/Atm4x/neuromita-rust/pull/309), fork head `046b7caeb24ac815c2d422339c9960b49f767b5b`.
- This GUI slice depends on #309. The earlier Python analysis in `docs/01-functional/characters.md` and `docs/02-ui/character-library-and-maintenance-actions.md` described a 2026-10-30 snapshot; the paths below were re-read at the pinned current Python commit.

## Confirmed user-facing capability matrix

| Function | Current Python behavior (pinned callsites) | Pre-existing Rust Core | Pre-existing Slint UI | Tests / current change | Remaining gap |
| --- | --- | --- | --- | --- | --- |
| Registered catalog / stable identity | `character_manager.py:42-56,114-115`: nine defined IDs, lazy runtime | `identity.rs`, `character/model.rs`: nine canonical IDs | `ui/characters/catalog.slint` + Core-fed entries, portraits | Existing catalog probe; no new catalog implementation | Python lazy config/history materialization differs |
| Active chat vs configured character | `character_settings_logic.py:275-298,684-706`; `character_settings_view_model.py:11-35`: independent IDs | Request/character API uses explicit `CharacterId` | Settings selection and chat selection already separate | New visual indicator of active chat ID; dirty switch guard | Stress-test late independent updates |
| Prompt-set selection and metadata | `character_settings_logic.py:818-879`: selected pack, info, reload | `PromptCatalog` list/resolve/render; `CharactersSettings.prompt_set` | Picker and metadata existed, each click previously saved immediately | Staged draft + single Core save; validates actual pack exists | Live pack reload after committed selection vs reload request still needs parity test |
| Per-character provider | `character_settings_logic.py:1040-1047`: `CHAR_PROVIDER_<id>` | Typed per-operation preset slots, `SettingsOwner` | Common/split preset picker existed | New Save/Cancel/dirty/CAS workflow; single owner transaction | Rust typed preset slots differ from Python single provider key; record explicitly |
| Character display name, descriptions | Built-in registry classes; production settings UI does not establish free-form edits | Built-in `CharacterDefinition` metadata | Read-only localized name and ID | No invented edit control | Confirm the separately supported override contract before adding write API |
| Custom fields/formulas | `character_config_manager.py:145-209`: read/validate `config.json` custom params; not a GUI profile editor | Runtime config/prompt DSL support differs | No established user editor | Left unchanged | Define safe schema and runtime binding in separate acceptance |
| Avatar and missing resources | Character assets shown by Python GUI | Built-in asset/definition handling | Placeholder via `CharacterCatalog.empty-avatar`; absent portraits for some IDs | Existing fallback retained | Real filesystem corruption/missing-asset headless and native tests |
| Settings persistence / restart | Python character and global settings are persisted | `SettingsOwner` + JSON repository atomic replacement | General persistence status existed | Core #309 unit test for isolation, CAS, invalid input, restart; new GUI save/retry observes Saved/Failed | Real file permission/partial write failure GUI E2E |
| Maintenance / mass destructive commands | Per-character and all-character history/index/backup actions, asynchronous and may partially succeed | Independent core services; no established matching safe GUI operations | Existing explicit unavailability labels | Left disabled deliberately | User-confirmed scope, progress, cancellation, report and backup semantics before enabling |
| In-flight LLM switch/cancel | Python explicit responder scopes and controller selection | Core request identity and cancellation exist | Chat target separate from settings draft | No new generation behavior in this UI slice | Add separate late-completion/cancel/return-to-previous integration tests |

## Design decisions

1. Keep `SettingsOwner` as the sole persistent profile authority; do **not** persist a parallel GUI JSON or per-character dictionary.
2. Scope drafts, commands, validation, conflict/error handling and persistence acknowledgement by stable `CharacterId`. The selected settings target never issues a chat-character switch.
3. A successful `save_profile` commits an **in-memory** revision only. The GUI leaves Save pending until an authoritative `PersistenceState::Saved` for the new values arrives, and shows failures with retry.
4. A nonempty prompt set is validated against the real `PromptCatalog`; a reference to missing resources is rejected instead of silently reported as saved.
5. Never add arbitrary character add/remove/import, name/description edits or custom-field mutation unless the running Python/UI contract and corresponding Core transactions are confirmed.

## Outstanding acceptance

- Core PR #309: CI core checks succeeded for `046b7caeb24ac815c2d422339c9960b49f767b5b`; additional disk-failure and simultaneous-generation acceptance remains.
- GUI (this PR): source-level and headless GUI checks must pass before Ready for Review; native interactive GUI session is not available via this GitHub connector.
- No automatic closure of [#20](https://github.com/Atm4x/neuromita-rust/issues/20) or [#39](https://github.com/Atm4x/neuromita-rust/issues/39).
