# PostDSL and request admission — 2026-11-05

## Ownership and execution order

1. Request ingress captures an immutable `SettingsSnapshot` and resolves the selected `GenerationPreset` before the request is enqueued. Pending requests never re-resolve generation settings from later UI changes.
2. `CharacterHandle` prepares a Timeline-backed character snapshot. `PromptPreparation` uses the resolved PromptSet and loads a request-scoped PostDSL program alongside the prompt, before generation.
3. The provider parses its structured response. The provider-neutral response generator runs the pure PostDSL program before constructing a `TurnProposal`. No character state is changed while generating or post-processing.
4. On acceptance, `CharacterHandle` validates all effects and commits the reply and `CharacterVariablesChanged` as a single Timeline append batch. Cancellation before acceptance leaves character variables unchanged.
5. Replay reconstructs variables per actor and Timeline branch from those events; there is no independent PostDSL variable store.

## Supported PostDSL

- `PostScripts/main_rules.postscript` per character PromptSet, independently selectable.
- Rules: `MATCH TEXT`, `MATCH REGEX` with optional `CAPTURE`, and `MATCH FIELD`.
- Actions: `SET`, `SET LOCAL`, `LOG`, `REMOVE_MATCH`, `REPLACE_MATCH WITH`.
- `DEBUG_DISPLAY` definitions and their resolved `debug_values` are available from the immutable program for presentation; the live GUI debug-value panel is **not connected** yet.
- `config.json` `custom_params`: `name`, `change_command`, `type`, `initial`, `op`, `formula`, `change_min/max`, `max_change`, `min/max`.
- Python-compatible processing order: text/regex across spoken segments, then custom parameters, then FIELD rules. `MATCH FIELD` receives the **original model field value**, including where `custom_params` has already processed the same field. In that situation both effects apply, as in the Python implementation.
- `SET LOCAL` belongs only to an invocation; it never becomes a persisted character variable.
- `LOG` expressions return bounded diagnostic lines (at most 256, 512 characters per line) and emit at debug tracing level only after successful PostDSL evaluation.
- Rule execution is bounded to 10,000 matched occurrences per response; regex capture names must match actual groups.

## Deliberate safety boundaries / known differences

- Unknown PostDSL expressions, unsafe function calls, unsupported actions, malformed scripts, and unhandled model custom fields cause the request to fail rather than silently skipping rules. The Python engine often logs an error and continues.
- Structured custom fields are consumed before persisted response creation; accepted character variables are private Timeline events.
- Custom variable updates are not applied on a cancelled, rejected, or otherwise unaccepted response. This is intentionally stricter than some Python imperative paths.
- Capture defaults and supported arithmetic match the shipped Python templates tested here, but this is not a blanket promise of arbitrary Python expression-language equivalence.
- Configuration snapshot happens at **admission**; a later edit to the underlying prompt files can affect a request if it has not entered `PromptPreparation` yet. The PostDSL program becomes immutable once preparation completes. Pinning an entire on-disk PromptSet version at admission is a separate capability.

## Validation

- `cargo check --offline --all-targets`
- `cargo test --offline --lib -- --skip ai_runtime::assets::generation::tests::approved_installer_stages_verifies_and_publishes_only_declared_assets`
- `cargo test --offline --test timeline_conversation --test vertical_slice --test contracts`

The skipped installer test needs an unavailable Python 3.12 executable; its failure does not test PostDSL.
