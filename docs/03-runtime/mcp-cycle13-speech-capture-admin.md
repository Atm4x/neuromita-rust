# MCP as GUI-peer speech capture administration — Cycle 13

The MCP endpoint is an **administrative peer of the GUI**. Speech sessions are created by the same application-owned `SharedSpeechCapture`, backed by the existing `SpeechInputHandler`, `AudioInputOwner`, `SpeechSessionController` and activated ASR worker. The GUI uses the same bridge. There is no second recognizer or distinct transcript dispatch path.

## Operations

- `presenter_speech_capture({"enabled":true,"character":"Crazy","selection_revision":0})`: starts the real microphone capture for the specified selected conversation. The agent should obtain the current UI selection revision; the GUI validates both `character` and `selection_revision` before applying recognized text to the selected chat. Requires `--allow-write` and an activated ASR worker. Optional `ffmpeg` and `ffplay` are existing **absolute** executable paths for non-native capture backends; if omitted, only configured native capture can start. Use `press_on_start:true` for radio/push-to-talk modes when starting from the chat toggle.
- `presenter_speech_capture({"enabled":false})`: stops the same capture; cannot supply character, revision, paths or `press_on_start` on stop.
- `presenter_speech_session({"pressed":true|false})`: controls PTT on the same currently active session; does not implicitly launch a worker.
- `inspect_presenter({"surface":"speech"})`: phase `stopped/starting/listening/stopping/failed`; no transcript or raw audio.
- `presenter_speech_transcript({"include_content":false})`: last accepted transcript revision, character, selection revision, language, text length; no text by default.
- `presenter_speech_transcript({"include_content":true})`: same plus transcribed text, requiring MCP launch flag `--allow-content`.
- `presenter_wait({"target":"speech_transcript","after_revision":0,"timeout_ms":30000})`: waits on the application-owned watch channel; returns revision + route identity only, no text.

## Concurrency and ownership

`SpeechTicket::admit` ensures each transcript dispatch is admitted once; invalidated/cancelled tickets do not publish. A bounded broadcast (64 items) is consumed by the real GUI runtime, while a watch channel holds only the most recent transcript for bounded MCP inspection. The GUI's existing selected-chat fence rejects transcripts from a previous selection, including a session started by MCP with stale revision. Both interfaces share a single capture owner: starting a second microphone session returns `AlreadyRunning`, and stopping it terminates the actual owner.

MCP tool permission boundaries remain `--allow-write` for start/stop/PTT and `--allow-content` for recognized words. Neither flag is TCP authentication. Audio data is not serialized in IPC.

## Verification (Rust 1.100.0)

- `cargo check --locked --offline --features mcp --all-targets`: PASS.
- `cargo test --locked --offline --features mcp --test local_control -- --test-threads=1`: 46/46 PASS. Includes real LocalControl rejection for uninitialized ASR, stop operation, transcript observation and shared ticket fence.
- `cargo test --locked --offline --features mcp --bin neuromita-agent-mcp`: 4/4 PASS.
- `cargo fmt --package neuromita -- --check`, `git diff --check`: PASS.
- The dedicated library-unit test for `transcript_bridge` is compiled with all targets but **not separately confirmed executed** due heavyweight library-test linking. The corresponding integration test passed.

These are **tool/owner contract tests**, not a test with a real microphone, audio driver, ASR model, or rendered Unity/GUI.

## Parity register

The baseline is the 68 `Command` variants in `src/bin/neuromita-gui/runtime.rs`. `SpeechSessionEnabled` and `ChatSpeechToggle` are now supplied through this shared capture API. Previous 42 fully / 12 partial / 14 absent becomes **44 fully / 12 partial / 12 absent**. Remaining `SaveTts` partial GUI/Telegram update and Telegram/Remote Voice, Knowledge Graph, character/preset/editor residuals are not affected.
