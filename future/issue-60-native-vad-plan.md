# Issue 60: native VAD evaluation and audio ownership reservation

Status: priority accepted, source research complete enough to start the first increment.
Worktree: `.worktrees/issue-60-current`; branch: `codex/issue-60-native-vad-current`.
Baseline: upstream/main `15710db73adea787e9ddb23a3ea2550ded96d260` (2026-11-03).
Issue: https://github.com/Atm4x/neuromita-rust/issues/60

## Scope and coordination

Evaluate real wavekat Earshot and optional Silero against the same bounded recorded-audio corpus, then establish one Rust owner for segmentation and input gating. Python remains an ASR provider, never a second authoritative segmenter. No second AI supervisor, generic RPC registry, implicit model installation or microphone recording. Cargo is reserved by parent/#51; no Cargo invocation until an explicit slot handoff. Parent review precedes push.

Issue 33 is saved at local merge `27a2f03d`, with clean worktree and no Chess implementation or dependency/vendor changes. It is paused. Do not modify issue 33 or issue 53 worktrees.

## Verified source baseline and gaps

- `src/audio/vad.rs` already uses Earshot directly, PCM16 mono 16 kHz / 256 samples. `examples/vad_smoke.rs` checks silence/sine, finite scores and reset; it does not measure speech accuracy. Wavekat is not yet integrated or evaluated.
- `src/audio/segment.rs` owns prebuffer/start/end/min/max policies behind `VoiceGate`. Segment output currently contains samples only; there is no utterance identity, source/client generation or capture epoch. Frames are tied to Earshot's 16 ms quantum.
- `src/audio/session.rs` has a bounded two-segment queue, explicit queue-full failure, cancel tokens and playback suppression. Reset cancels the active turn but does not invalidate already queued segments. Capture EOF drains queued work without an explicit deadline. Local playback suppression is not a general speaking lease. These are source findings requiring regression tests, not claims of observed runtime failures.
- `Application::speech_session` is only available with configured Python ASR. Native VAD evaluation must run without that installation. `ChatReplyTarget` uses static ChatApi; delivery must remain bound to the original owner/client/session through cancellation and restart.
- `src/audio/device.rs` owns FFmpeg/ffplay processes and cancellation-safe partial PCM reads. Device selection is explicit, but enumeration/hotplug/reopen/generation ownership are not implemented there. This is Rust-owned capture through an external adapter, not proof of a native hardware driver.
- `tests/audio_pipeline.rs::microphone_vad_asr_player_message_generation_tts_playback` uses fake capture/gate/transcriber/playback. Its name is not physical microphone or real-ASR evidence.
- GUI has an ASR section entry and a static microphone-off indicator; these are not connected capture lifecycle/readiness controls. Keep UI work limited to actual scope: truthful state/error presentation and explicit input controls if required, not a redesign.
- Current remote main remains `15710db7`; merged PR 71 includes the existing audio baseline. Remote `bridge/ai-audio-*` refs are older transport/provenance branches, not a newer independent VAD implementation. No separate active VAD PR appeared in the checked latest 100 PRs; recheck before publishing.

## Resources and evidence boundaries

Targeted scans found no WAV/FLAC/MP3/OGG corpus in Rust assets/tests/examples/python or tracked Python source/assets/tests, and no Silero/VAD ONNX file in the targeted Python tree. This is not a machine-wide asset search. FFmpeg and ffplay resolve through WinGet PATH links. Read-only Windows endpoint registry includes an active USB Microphone endpoint. Parent additionally reports successful read-only FFmpeg DirectShow enumeration of USB Audio Device, USB Microphone and Steam Streaming/Oculus virtual devices; this invocation required escalation past the sandbox. Enumeration alone proves neither recording permission nor usable samples. No capture, playback, inference, model download or Cargo check has run for this investigation.

Disk budget reported by parent: about 3.9 GB free. No large models/corpora. James independently researches tiny public RU/EN speech/noise fixtures and Silero model license/hash. Before acquisition, require exact URLs/revisions, per-file license/attribution, size cap and SHA-256; approve only a few MB of selected clips. Preserve original bytes and document deterministic conversions, channel/rate/trim, source transcript and manually reviewed speech boundaries. Avoid treating synthetic noise or silence as real-world noise acceptance.

## First implementation increments

1. Add a reproducible file-based evaluation harness with bounded RIFF/PCM input and explicit fixture manifest. Record frame scores, predicted segments, speech-boundary losses, false-positive duration, precision/recall, endpoint latency and local timing separately. Use identical clips/policy for both providers; never present upstream benchmark numbers as local results. Freeze annotations before tuning and retain a separate evaluation subset.
2. Evaluate wavekat Earshot with default features disabled and only `earshot` enabled. Review/pin the released crate and intentional dependency delta first; existing vendor bytes remain unchanged. Compare direct Earshot and wavekat output/reset on real clips to decide whether the wrapper belongs in production. No new voice crate or device backend without an observed capability gap.
3. Evaluate optional Silero CPU with a verified explicitly selected local asset and runtime. Wavekat upstream currently uses 512 samples / 32 ms at 16 kHz for Silero versus 256 / 16 ms for Earshot. Add explicit frame accumulation and reset semantics; never pad or splice across capture epochs silently. The `silero` build script otherwise downloads its model; local `SILERO_MODEL_PATH` plus verified content is mandatory. Review ONNX Runtime dependencies and offline DLL provisioning before enabling this feature. A runtime `from_file` API alone does not disable build-time downloads. GPU testing is not required for CPU VAD evidence and will not be claimed.
4. Strengthen the existing segment/session owner rather than duplicating it: typed capture epoch, utterance identity, client/session and press generation; invalidate queued work on reset/rebind/disconnect; reject duplicates and late ASR delivery; explicit bounded stop/drain/error outcomes. Keep gate/prebuffer/hangover/length policy independent of the score provider. Match Python gate/session behavior intentionally, including radio/PTT, permission and speaking-lease transitions.
5. Keep FFmpeg capture initially. Add a typed device/lifecycle port and minimum observability needed to test rebind/hotplug/error/reset. Select a new native device dependency only if this adapter cannot meet the accepted requirements. Production settings/presenter wiring must expose configured versus actual readiness/errors. Physical capture ownership transfers only after recorded-audio parity and explicit user-started microphone acceptance.

## Acceptance matrix

| Layer | Required evidence |
| --- | --- |
| Domain | Deterministic PCM/gate tests: prebuffer, start/end, min/max, held PTT, release/cancel, radio/client scope, stale/duplicate generation, speaking lease, epoch/reset and queue pressure. |
| Actual VAD | Real wavekat Earshot and real Silero on the same annotated RU/EN clips, pauses and recorded noise. Silero is an optional product feature, but testing it with a verified local asset is required for issue acceptance. Report local metrics, thresholds, versions, hashes, timing environment and failures. Mock tests supplement this evidence. |
| Session/lifecycle | Controlled stalled ASR, late completion, queued-segment invalidation, partial frame cancellation, EOF/drain deadline, disconnect/rebind, dropped owner and old-Core restart delivery rejection. |
| Production integration | VAD/file harness without Python installed; configured Python ASR receives only completed utterances with original routing identity. Missing runtime/model/dependency produces explicit readiness/error. No live model claims from fixture workers. |
| Hardware | Read-only device list first. User explicitly starts capture; then known RU/EN phrases, selected-device switch, unplug/replug, PTT/cancel and playback/speaking suppression. Keep this pending until actually performed. |
| GUI | Truthful readiness/error state and applicable controls; test presenter bindings independently. Real window/device acceptance is a separate evidence step. |
| Validation | After Cargo slot release: focused tests, then required default/no-default offline locked suites, GUI checks only if touched, Clippy warnings denied and fmt. Python 3.12 SDK tests if Python changed. Shared root target/debug0/incremental0 and agreed vendor path; no concurrent Cargo/test binary writers. |

No current test counts or real microphone/GPU acceptance are claimed. Remaining asset/version/checksum details depend on James's review; implementation need not wait to prepare domain regression cases and the manifest/harness design.
