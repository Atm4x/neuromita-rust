# Preset selection at generation execution

## Invariant

`ChatApi::reply`, `ChatApi::react`, and `ChatApi::initiate` express *what to do*, never a provider, model, preset or transport configuration. A pending `GenerationCommand` stores only character, operation, target/instruction, source and audience. It does **not** resolve or retain a preset at admission.

At exactly one point, **after dequeue and immediately before beginning LLM generation**, the generation executor reads the currently configured preset assignment for `(CharacterId, operation)` and the active preset ID. It resolves the ID against the configured preset catalog, **materializes/clones a fully owned `GenerationPreset`**, then supplies that value to `GenerationRequest::new(context, preset)`. No additional preset settings reads or re-resolution occur for that attempt, including retries.

- Queued commands pick up edited assignments and active presets on their eventual execution.
- A running command retains the preset value selected at its start even if assignments or preset definitions change later.
- Different operations (`Reply`, `React`, `Initiate`) can have different assigned preset IDs for the same character.
- `Active` is a tagged selection, not a magic integer ID. A specified but missing ID is an explicit failure; never silently fall back.
- `Character` and `SessionTimeline` never own or choose presets; this is application configuration and executor policy.
- Built-in character assignment defaults use an exhaustive `match` on `CharacterId`, so adding a new variant requires compiler-visible maintenance.
- No additional version reconciliation, background synchronization or per-request settings subscriptions are required for this selection rule.

## Scope of initial implementation

The existing CLI-configured Gemini preset seeds the catalog's default ID. `AppSettings.presets.active` owns the active ID, while `AppSettings.characters.<character>.presets` stores per-operation selections. These are independent typed settings sections; preset definitions remain in the separate `PresetCatalog`. The serialized settings schema v2 migrates older `presets.characters` assignments without changing their values. Preset catalog CRUD, storage of provider credentials, alternate protocols and UI editing remain part of Issue #16; they must not be simulated by unsafe plaintext credential persistence or a second ChatApi entry point. This contract also does not alter Unity transport or structured response.

## Editable catalog (2026-11-02)

A preset *definition* is a standalone user configuration, not a field in `AppSettings`. `PresetRecord` owns a name and validated provider definition; `PresetCatalog` owns stable IDs and `create/update/delete/list`. The optional `PresetRepository` stores a versioned catalog JSON separate from application settings, and the JSON repository writes via a temporary file and rename. Failed persistence does not publish an in-memory edit. IDs are not reused after deletion.

Gemini definitions store the **name** of an environment variable (`credential_env`) rather than an API key. Credentials are resolved only when materializing the preset for a queued generation. An absent credential fails explicitly; it never becomes a fake request or an implicit fallback to another preset. `EnvironmentPresetCredentials` is the default; an injected `PresetCredentials` allows deterministic tests. Definitions are validated at CRUD boundaries. `PresetCatalog::open(runtime, repository, credentials)` loads persistent definitions and may reserve runtime-only bootstrap entries for the current CLI; runtime bootstrap entries cannot be edited through catalog CRUD.

`PresetApi` exposes catalog operations to application clients; it does not own character assignments or call `ChatApi`. A running generation continues to hold its cloned `GenerationPreset` after a catalog edit or deletion; a pending generation sees the current catalog on its one resolve point.

**Remaining #16:** connect a persistent repository in the user-facing launcher/runtime bootstrap (the `minimal()` CLI still uses an ephemeral seed); UI editor/presenter, credentials management beyond environment references, additional protocols and real model-discovery/connection tests. Catalog CRUD here is not a claim that Issue #16 is complete.
