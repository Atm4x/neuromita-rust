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

The existing CLI-configured Gemini preset seeds the catalog's default ID. The settings section owns the active ID and per-character/per-operation selections. Preset catalog CRUD, storage of provider credentials, alternate protocols and UI editing remain part of Issue #16; they must not be simulated by unsafe plaintext credential persistence or a second ChatApi entry point. This contract also does not alter Unity transport or structured response.
