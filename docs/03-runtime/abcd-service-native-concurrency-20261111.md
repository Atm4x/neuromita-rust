# ABCD — Independent service IPC and generic model identity (2026-11-11)

This iteration continues PR #416 after `ae2e7a1a24a6afc6e863cdf33ee617c6b8f1f5c5`. It does not change the one-active-model-per-service invariant. Model implementation is **catalog metadata**, never a Python/Rust user toggle. Two identically named entries of different implementations are separate identities. A service resolves one concrete identity, and the matching implementation owns its worker.

## Ownership hierarchy

```text
ApplicationLifecycle
  ServiceWorkerRegistry
    one exclusive worker claim per AiService (TTS, ASR, Embedding, Reranker, Beats)
    service-specific inference read permit / transition write gate
  AiHubNativeService
    short-held catalog mutex: signed selections, installed revisions, active session handles, downloads
    TTS: Arc<NativeSpeechSession> -> own Tokio mutex -> own NativeModelController + child + IPC
    ASR: Arc<NativeSpeechSession> -> own Tokio mutex -> own NativeModelController + child + IPC
    generic historical native session (not yet a typed service)
```

`AiModelIdentity` is an independent Core type with `model_id`, `implementation`, and optional `revision`. Persistence `SpeechModelReference` converts into it at the boundary; `ServiceWorkerRegistry` no longer owns a speech-settings DTO. This makes the same identity and exclusive worker-claim mechanism usable for Embedding/Reranker/Beats, but it **does not** register corresponding model processes or implement their typed inference contracts.

## Process lifecycle and fences

- The selected signed revision is resolved once into `PreparedSpeechWorker`. Its model identity and manifest SHA-256 are pinned together; a later on-disk selection cannot redirect this process.
- The service owner acquires the transition gate, reserves its exclusive claim **before process startup**, performs the expensive startup outside the shared catalog lock, then atomically publishes the completed session. A failed startup drops its claim.
- A native TTS or ASR request holds a typed `ServiceInferencePermit` for the **correct registry and service** until the worker returns and its selected-model snapshot is checked. Wrong-service and foreign-registry permits cannot authorize dispatch. A concurrent transition must wait for the permit.
- Inference takes the shared catalog mutex only to clone an `Arc` handle. Worker exchange runs under the **session's own** mutex; another service's process cannot block it through the catalog lock.
- Controller observations are published via `tokio::sync::watch`; GUI snapshots read the last observed state without locking or waiting for a slow IPC exchange.
- Per-service deactivation waits for the service transition gate, obtains the session handle, releases the catalog lock, stops the process, then removes the exact matching session and drops the worker claim. If stop fails, the claim remains held (fail closed; no second process admitted).
- Polling refreshes TTS and ASR independently, with per-session locks rather than serialized requests under the catalog mutex.

The generic untyped legacy session is still handled separately and must not be treated as an approved TTS/ASR/RAG service worker. The concurrent workflow APIs preserve older generic tests without inventing service capabilities.

## Regression and evidence

`tests/native_speech_protocol.rs` uses a real Rust fixture program with two distinct OS processes from the same digest-pinned schema-v2 test package. The TTS process intentionally holds a request until a release file is written. While TTS is held, the ASR process must complete its valid speech IPC within a bounded deadline. This proves independent dispatch **at the worker IO layer**, not merely independent Rust locks. The test always releases TTS before asserting the ASR result to avoid stranded fixture workers on failure.

`service_workers` also checks cross-service identity collision rules and verifies that permits cannot be reused for a different service or registry.

For final test counts and publication SHA, see the PR #416 iteration note and the generated report. This iteration uses Rust 1.98.1 and offline vendored dependencies; it does not claim a Windows/GUI build or real inference quality check.

## Still required before `main`

1. Apply the same typed service process lifecycle and operation-specific invocation contracts to Embedding, Reranker, RAG and other AI services. A generic registry slot is only the ownership primitive, **not** a running model.
2. Consolidate model catalog and Slint presenter into a single model list with distinct implementation identities, **without Python/native switches**.
3. Resolve external SQLite-writer RAG consistency using an explicitly defined writer/reader serialization protocol; pre-call freshness checks alone cannot close a concurrent commit window.
4. Validate actual signed worker packages, Windows GUI and Unity, full localization, strict Clippy, and recovery/cancellation under load.
5. Classify or deprecate generic schema-v1 native inference before claiming full service coverage.

Keep PR #416 Draft until acceptance is complete. Never force-update `main`.
