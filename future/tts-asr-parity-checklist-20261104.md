# TTS / ASR Python → Rust parity — 2026-11-04

## Confirmed code contracts

- [x] Google/Whisper/GigaAM language catalogs shared with Python, selected language validation.
- [x] Desktop ASR transcription routed through the chat draft and immediate/delayed send preferences.
- [x] Separate persisted TTS/ASR plugin overrides in the existing SettingsOwner.
- [x] Dynamic Slint forms for Python `settings_schema` scalar types (`string`, `boolean`, `integer`, `number`), scalar choices, defaults and numeric bounds.
- [x] Asynchronous, service-checked schema inspection by a temporary Python host; no install/probe/inference; stale responses are discarded.
- [x] Edits validated against inspected schema, persisted for the selected plugin only, and passed to existing activation resolver. Explicit reset clears stale options after schema updates.
- [x] Failed or unavailable schema inspection is reported in the GUI; it never fabricates model readiness.

## Remaining parity gaps

- [ ] Telegram TTS and remote/API TTS: implement actual service adapters, lifecycle, secrets handling and verified inference, instead of only settings UI.
- [ ] Game-owner routing: forward captured ASR text to the current game-session owner, mutually exclusively with desktop chat, with capture/session identity.
- [ ] Game delivery idempotency: ensure recognized utterances are not resent after reconnection or failed acknowledgements.
- [ ] External speech gate: suppress microphone recognition while a Mita character is speaking in the game, not only during local playback.
- [ ] ASR deduplication: normalize and suppress recent exact and near-duplicate utterances per Python rules.
- [ ] End-to-end mock worker tests for application startup, model switch, ASR capture, transcript routing, TTS and Slint bindings; no physical devices required.
- [ ] Make plugin metadata accessible in packaged deployments before the managed Python environment is prepared: current contract uses an installed managed interpreter or explicit `NEUROMITA_AI_METADATA_PYTHON` absolute path, never silently downloads Python.
- [ ] Integrate optional Python `configuration_constraints` (hardware-specific restrictions) with form editing. The standard `settings_schema` field constraints are implemented; final runtime validation remains authoritative.

## Ownership and limits

- Model installation is exclusively an AI Engine concern, never a TTS/ASR GUI action.
- Plugin schema comes from the Python SDK; Rust implements a typed bounded view/validator for GUI inputs and stores only user overrides via `SettingsOwner`.
- The runtime validates configurations again before activating a model, including installed layers and assets; a GUI schema alone never marks a model installed or initialized.
- Existing Presets API and legacy Python scripts are not changed.
- Verified by code compilation and mock Python contract tests, not a live GPU/microphone session.
