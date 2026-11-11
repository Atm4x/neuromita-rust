# ABCD — Explicit native speech routing and signed capabilities (2026-11-11)

Base: `e8a3143a6d0ba87306234d034c0cb11682672ce3`, PR #416. Scope: the native inference contract and the Core speech dispatch boundary. This is not a claim that a production native TTS or ASR executable has shipped.

## Ownership

```text
ApplicationLifecycle / exclusive profile lease
  └── ApplicationOwner
       ├── persisted SettingsOwner (Python | Native { model_id, manifest_sha256 } per TTS/ASR)
       ├── LiveSpeechRuntime (Python worker orchestration)
       └── AiHubNativeService (trusted native session + shutdown)
            └── NativeModelWorkflow
                 └── NativeModelController
                      └── NativeModelProcess (single correlated IPC session)

Core VoiceProvider / SpeechTranscriber
  └── RoutedSpeechVoice / RoutedSpeechTranscriber
       ├── explicit Python selection → Python owner, **no native fallback**
       └── explicit Native selection → Core native owner, **no Python fallback**
            ├── active session `model_id` and manifest SHA-256 must match the selected exact revision
            ├── verified, manifest-pinned TTS/ASR capability + language
            ├── typed versioned payload + bounded canonical PCM16 WAV
            └── settings selection checked again after inference
```

## Manifest and worker attestation

Legacy `native-model.json` schema **v1** remains generic-inference only. Its serialized representation and existing package digests are preserved: absent speech fields are omitted on serialization. A speech release must use `schema_version: 2`, `protocol_version: 1`, `speech_contract_version: 1`, and between one and two distinct `speech_capabilities` entries. Each entry declares exactly one of `tts`/`asr`, a PCM WAV sample rate (8000–96000 Hz) and a bounded, explicit language set. Invalid duplicate, unsupported or undeclared capabilities fail registration.

The package manifest is SHA-256 pinned by the existing trusted release admission. `NativeModelProcess::start` independently compares worker-reported speech contract and capabilities with the pinned manifest **before** loading any model. A schema v1 worker advertising speech capabilities, or a v2 worker whose capabilities differ from the pinned manifest, is rejected with `Protocol` and terminated.

Typed speech still uses the existing correlated JSON-lines transport (`command: infer`, version 1). Speech payloads additionally require `speech_version: 1` and `kind: tts|asr`. TTS requests include text, voice, language and volume_percent. TTS replies are bounded base64 canonical PCM16 WAV with the declared sample rate. ASR requests include canonical PCM16 WAV (base64), sample rate and selected language. ASR replies include a bounded transcript and optional language. Unsupported languages, sample rates, frame sizes or malformed responses fail closed. Generic/legacy IPC retains a 1 MiB frame limit. Typed, manifest-attested speech infer calls alone allow up to 24 MiB IPC frames, with a separate 8 MiB WAV payload limit.

## Explicit source selection

`SpeechSettings.tts_engine` and `.asr_engine` are typed settings: `{"engine":"python"}` or `{"engine":"native","model_id":"...","manifest_sha256":"<64 hex>"}`. Legacy settings lacking these keys load as `Python`. Existing plugin selection/inspection skips Python preparation for services explicitly routed to native; the Python live-runtime selection key ignores those services. A native request cannot silently choose a Python plugin whose ID happens to match the native model ID.

Activation remains an independent AI Hub action: selecting a speech engine **does not** automatically install or launch an executable. The native service must already have activated an appropriately signed model revision in the same application-owned profile. A missing/inactive model or mismatched capability returns an error; it does not attempt an install, switch model or fall back. One active native worker exists under `NativeModelWorkflow`, so simultaneous native TTS/ASR currently requires a single model advertising both services. Independent native TTS and ASR workers are a separate change.

## Remaining acceptance gates

1. GUI/presenter must expose the explicit TTS/ASR engine selectors and correct status/readiness projections; typed settings are persisted and enforced at Core boundaries, but not yet selectable from the normal Slint controls.
2. Integration with production signed schema v2 speech releases and actual native GPU inference still requires real models/workers. The fixture executable tests the protocol, not inference fidelity/performance.
3. The existing single native worker cannot host two distinct native ASR/TTS model IDs simultaneously; multi-worker orchestration, service-scoped replacement and coordinated shutdown are future work.
4. Windows GUI build/manual acceptance, real Unity, full strict Clippy, full English catalog and transaction-level RAG guarantees with external SQLite writers remain open. No direct edit to `main` is authorized.

## Test plan

- Check `cargo check --lib --offline`, `cargo check --tests --offline`, `cargo fmt --all -- --check`, `git diff --check` on Rust 1.98.1.
- Exercise schema v1 compatibility and schema v2 manifest validation.
- Exercise fixture native speech worker attestation, real stdio IPC TTS/ASR, mismatched worker claims, malformed audio/language and non-fallback native selection.
- Run focused native and speech tests; full test suite and GUI build if resources permit.
