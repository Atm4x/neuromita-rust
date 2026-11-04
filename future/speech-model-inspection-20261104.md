# Speech plugin inspection and activation resolution (2026-11-04)

Base: main `2dcf12774a526c6a987770fd1273c2f372d19e9f` (includes #82 GUI integration).

## Boundaries

- `SpeechSettings` owns per-plugin overrides for each service. `SettingsPresenter.tts/asr.model_options` provides the existing observable, atomically persisted settings boundary.
- `SpeechModelCatalog` remains the single read-only file/receipt inventory. No duplicate inventory database is created.
- `select_speech_model` selects the configured plugin and a single verified revision (ambiguous revisions are errors). Telegram and API TTS do not select local workers.
- `fetch_speech_plugin_inspection` uses a short-lived Python SDK host, reads `discover`, `settings_schema`, `validate_config`, `install_plan`, `probe_install`, then stops. Python never initializes the plugin model and never installs packages from this API.
- For a selected revision, the existing `ModelAssetStore` validates it before exposing it to the inspector process. Dependencies are inspected in caller-supplied isolated layers, not implicitly in an arbitrary Python environment.
- `inspect_speech_selection` only returns `ModelActivation` when the plugin's probe explicitly reports installed and any required model assets have a verified revision. Error and missing state are never reported as ready. The plan is returned to the application orchestrator, which remains responsible for actual activation.

## Explicitly outside this change

- No new installer button or SDK calls inside Slint UI.
- No microphone activation or inference worker start from the settings page.
- No running-worker status observation yet; a successful installation probe does not prove that initialization/inference succeeds.
- The dynamic schema is now retrievable and validated in Core, but the full form renderer for arbitrary third-party model options is still pending.

## Validation

- `cargo check --locked --offline --lib` passed.
- Speech tests validate schema/probe errors, model selection, default activation mapping and legacy persistence.
- One existing global library test requires Python 3.12; the test environment currently exposes Python 3.13, so full-suite success cannot be claimed.
