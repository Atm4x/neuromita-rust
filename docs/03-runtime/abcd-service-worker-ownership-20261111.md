# ABCD — Service-owned model worker architecture (2026-11-11)

Base: `2af69d4886c23a5c06d1619255ff4097f7c2f277`, PR #416. This iteration supersedes the *engine selection* described by `abcd-native-speech-routing-20261111.md`. That earlier document is historical, not a current contract.

## Canonical identity and execution

A user selects a concrete **model entry**, not an execution language. A model entry includes `{ model_id, implementation, revision }`. Two entries may share `model_id` and display name while differing by implementation (Python vs Rust) and signed revision. The implementation is immutable catalog metadata. A selected model is bound to exactly one service (`TTS`, `ASR`, etc.) and one implementation. No silent backend fallback, name-based runtime guessing or Python/native selector is accepted at the inference boundary.

Persisted speech settings now contain optional `tts_model` and `asr_model` references. Legacy profiles without these references resolve their existing Python plugin IDs. Previously serialized `tts_engine`/`asr_engine` fields are recognized *on read only* and migrated into model references, with the old engine keys omitted on write. Native references pin the signed manifest SHA-256.

The `TtsWorker` and `AsrWorker` contracts in `native_speech.rs` dispatch by an implementation registry. Concrete Python and Rust implementations provide identical service operations to their callers. The runtime implementation is not selected by `if/else` in a voice request. Unsupported or inactive implementations fail closed.

## Service-level ownership

```text
ApplicationLifecycle
  └── ServiceWorkerRegistry
       ├── TTS slot (0 or 1 active model/process)
       │    ├── PythonTtsWorker implementation
       │    └── RustTtsWorker implementation
       ├── ASR slot (0 or 1 active model/process)
       │    ├── PythonAsrWorker implementation
       │    └── RustAsrWorker implementation
       └── Embedding / Reranker / Beats (registry slots, adapters pending)

Service slot:
  model reference -> exclusive worker claim (RAII)
                  -> activation -> inference read gate
                  -> exclusive stop/swap gate -> process shutdown
                  -> claim release
```

`ServiceWorkerRegistry` claims are keyed by service, not by language or model ID. Activating another implementation or revision for the same service fails before spawning. Different services can hold workers concurrently, even when their model IDs are identical. Service-scoped `RwLock` gates prevent shutdown/replacement from racing a provider call and its post-dispatch selection verification.

Python `LiveSpeechRuntime` now shuts the old application worker down **before** activating its replacement. It acquires claims before startup and releases them after confirmed shutdown. This intentionally changes the former startup-candidate-first behavior; keeping old and new models alive concurrently is not allowed. A bad preparation plan is rejected before stopping the old worker, but a failure *after* it is stopped no longer resurrects it.

`NativeModelWorkflow` owns separate TTS and ASR sessions. `activate_for_service(service, model_id)` and `deactivate_for_service(service)` operate on one slot; TTS and ASR may run distinct signed model processes. Models advertising both capabilities must be activated under an explicit service; a generic activation cannot silently choose a service. Generic schema-v1 native test packages retain their historical untyped entry point, but cannot supply typed speech inference. Shutdown joins and releases all native speech sessions.

The **normal** `VoiceProvider` / `SpeechTranscriber` verifies that the active claim exactly matches the selected model identity before and after external inference. A valid service worker cannot be silently substituted with a same-named model from a different implementation.

## What is not claimed complete

1. The application GUI still exposes the historical Python catalog and a separate native package panel. A unified model-card catalog (same model name, distinct implementation-backed entries, selected as one model) and presenter controls must replace these views. There must be **no Python/native toggle** in the GUI.
2. The native workflow currently protects a shared workflow-state mutex across TTS and ASR infer calls. They are separate processes with separate service claims, but their request scheduling can still serialize. Per-service IPC request mutexes must be separated without weakening lifecycle fencing.
3. The generic AI worker abstraction is not yet applied to embedding/RAG/reranking and other services. These need the same metadata and service-owned lifecycle, without introducing duplicate active workers.
4. Legacy native schema-v1 generic inference is intentionally not registered as an active typed service worker. It should be classified or retired as typed service implementations become available.
5. Windows GUI/real signed workers and external-SQLite writer consistency remain separate acceptance gates. PR #416 remains Draft and must not be merged into `main` on this iteration alone.

## Verification

Run with Rust 1.98.1 and the repository's offline vendored dependencies: `cargo fmt --all -- --check`, `cargo check --tests --offline`, `cargo test --lib --offline`, `cargo test --test native_revisions --test native_speech_protocol --test native_model_registry --offline`. Check the latest CI and run results in the PR comment; tests do not prove real model inference quality.
