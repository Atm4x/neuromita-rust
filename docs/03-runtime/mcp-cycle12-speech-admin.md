# MCP / GUI speech administration — Cycle 12

This contract equips the main acceptance agent with **actions against the actual application-owned Presenter and speech controller**. It does not assert that a model was activated, that microphone audio was captured, or that Unity/Slint rendered a result.

## Commands and ownership

`presenter_speech_settings` is a single typed MCP tool, requires `--allow-write`, and is forwarded through LocalControl `presenter_action`. All writes call the **same SettingsPresenter** as the desktop host, with the same model identity validation and fields preserved by the GUI controls:

| MCP action | GUI Command | Actual owner | Result |
|---|---|---|---|
| `select_model` | `SelectSpeechModel` | `SettingsPresenter.tts/asr.model` | canonical selected service/model, settings revision |
| `save_asr` | `SaveAsr` | `SettingsPresenter.asr.preferences` | settings revision |
| `select_asr_recognizer` | `SelectAsrRecognizer` | `SettingsPresenter.asr.model` | selected Python model, settings revision |
| `save_character_voice` | `SaveCharacterVoice` | `SettingsPresenter.tts.preferences` | affected character, settings revision |
| `save_tts` | `SaveTts` | `SettingsPresenter.tts.save_python_preferences` | settings revision |

`save_tts` preserves existing `default_voice_id` and `character_voice_ids`. `save_asr` preserves selected `recognizer_id` and its `model_languages`; optional `language` updates the selected recognizer's language. These actions **do not claim persistence completion**: use `presenter_settings_read(area=persistence)` or `presenter_wait(target=settings)` to observe the storage acknowledgement. A Rust model reference has the same visible name and carries its implementation/revision identity separately. The regular GUI `SaveTts` path also refreshes the active Telegram voice provider; the new MCP settings action does **not** yet refresh that live Telegram session. Count this GUI Command **partially covered**, until a shared Telegram owner is exposed to both adapters.

`presenter_speech_inventory` is read-only and rescans the **same configured AIData root** and microphone adapter as desktop `RefreshSpeechInventory`. Returns bounded TTS/ASR options, package verification, installed revision IDs, device identity/backend and discovery error count. It does not activate models, export raw plugin error strings or filesystem paths. A Core without an owned AIData root returns a precise unavailable error.

`presenter_speech_session(pressed=bool)` requires `--allow-write` and sends the `SpeechPushToTalk` control to the **application's existing SpeechSessionController**. The call cannot start a missing capture session: the owner refuses the press when no active ASR capture exists. Use `inspect_presenter(surface=speech)` to read the phase. `presenter_audio_input` remains the separate microphone loopback test and does not start ASR.

## Local verification

Tested using offline Rust 1.100.0. `cargo check --locked --offline --features mcp --lib --bin neuromita-agent-mcp --test local_control` succeeded, **43/43 TCP/Core integration tests** and **3/3 MCP unit tests** passed. Regression coverage includes typed route validation, forbidden unexpected fields, real SettingsPresenter mutation, preserving voices and recognizer identities, inventory without path exposure, and refusal to press an inactive session. This is **API-contract evidence**, not Unity/audio E2E evidence.

## Source-based GUI Command progress

This cycle starts from 34 complete / 11 partial / 23 missing GUI `Command` variants (68 total, as audited in `mcp-gui-command-parity-audit.md`). New full matches: `SelectSpeechModel`, `SaveCharacterVoice`, `SaveAsr`, `SelectAsrRecognizer`, `RefreshSpeechInventory`, `SpeechPushToTalk`. `SaveTts` advances to partial because of the missing live Telegram update. The remaining missing Speech commands are `SpeechSessionEnabled` and `ChatSpeechToggle`.

**Current GUI Command parity: 40/68 complete (58.8%), 12/68 partial (17.6%), 16/68 absent (23.5%). Audio/Voice/ASR/TTS: 11/14 complete, 1/14 partial, 2/14 missing. Total MCP tools: 36.** The 68-command denominator does not include all Slint callbacks, direct Presenter methods, or Unity host-only actions.

## Remaining architecture

1. Expose one shared speech-capture/Transcript routing owner; do not create a second ASR worker or silently change the GUI's transcript-into-chat policy. This is needed for `SpeechSessionEnabled` and `ChatSpeechToggle` and live transcript revision/wait.
2. Move Telegram voice session control from desktop runtime into an application-owned service or an injected port with the exact same policy and cancellation semantics; then close `SaveTts` completely and implement Telegram/RemoteVoice commands.
3. Continue Knowledge Graph, remote voice and launcher parity using their established presenters and typed fences. Direct Core mutation remains separately gated by `--core-debug`.

See [PR #446](https://github.com/Atm4x/neuromita-rust/pull/446) and [full-stack acceptance issue #442](https://github.com/Atm4x/neuromita-rust/issues/442).
