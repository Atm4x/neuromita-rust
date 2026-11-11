# ABCD integration: Core-owned AI Hub operations (iteration 2, 2026-11-10)

Base: `e9fee6938fdfe0f3b999c8e0ff00658b0065c448` (PR #416). This is a continuation of the runtime boundary audit, not authorization to merge the integration branch into `main`.

## Ownership and call tree

```text
ApplicationLifecycle (exclusive settings/profile lease)
  └── ApplicationOwner (shutdown ordering)
       ├── AiHubInstallCoordinator
       │    ├── one-use InstallApprovalGate and InstallReviewFence
       │    ├── Core-owned review JoinHandle (abort+join on invalidation)
       │    └── Core-owned active install JoinHandle (join on completion; abort+join on shutdown)
       └── AiHubNativeService
            ├── trusted NativeModelWorkflow (at most one active worker)
            └── NativeDownloadOperation (at most one transfer; cancel+join on shutdown)

Desktop runtime
  ├── typed command → Core owner method
  └── Core result/event → UI projection
```

Two independent Core child services are composed by *one* top-level application owner. Their roots must match the exclusive profile lease. Installation admission no longer lives in desktop runtime, and the native worker/download objects are never held by the GUI. Install worker tasks do not retain an `Arc` cycle back to their coordinator. When the GUI refreshes or changes its review selection, the Core authority invalidates the old approval and fences late completion events. Core rechecks the current model catalog at confirmation.

## Guarantees and deliberate limitations

- Approved plans are one-use; the GUI has no serialized install plan that can be replayed or edited to authorize work.
- Installing a Python wheel or overlay is not activating an inference worker; an installed native revision is not a ready session.
- The native publisher policy remains pinned by the build, not learned from model data. Untrusted package/origin requests fail closed.
- The Core joins a native download after cancellation, permitting atomic publication to finish if already committed.
- The Python installation task is aborted and joined at shutdown; install subprocesses are configured to terminate on future drop. A crash-power-cut durability acceptance is separate and must test interruption at every file-publication stage.
- `NativeModelProcess::infer` remains generic JSON IPC. Package manifests currently declare backends, not `Tts`/`Asr` service capabilities or typed audio contracts; **ordinary speech still uses the Python speech runtime**. Do not enable a silent native/Python fallback or treat an activated native model as valid for arbitrary speech.
- No global serializable lease exists across SQLite writers and remote RAG provider network operations. The dispatch fences from PR #416 remain useful but do not guarantee a foreign writer cannot commit after authorization.

## Acceptance still required

1. Define versioned native capability + typed TTS/ASR IPC schema (sample rate, audio format, transcript, language, model identity, error/cancel semantics) and enforce capability matching to selected settings in Core.
2. Reconcile Python/native service selection in the existing Core voice/ASR pipeline, with explicit user selection and no fallback unless explicitly configured.
3. Exercise real native worker, Python wheel/overlay install, failure during shutdown, Windows GUI, Unity loopback and importer/updater interactions.
4. Determine cross-process RAG writer coordination or explicitly document snapshot-time access policy; arbitrary foreign writers cannot be controlled by an unshared in-process mutex.
5. Repeat full test suite, strict Clippy and English localization completeness test on Windows before `main` acceptance.
