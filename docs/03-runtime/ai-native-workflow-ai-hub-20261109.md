# Native model Core workflow and AI Hub — 2026-11-09

This change integrates **already installed and externally approved native Rust model EXE packages**. It does not implement native RVC/Fish Speech inference, weight conversion, Python SDK deprecation, or trusted network acquisition.

## Ownership and call graph

```text
AI Hub Native tab (Slint view)
  -> desktop Command::{RefreshNativeModels, SelectNativeRevision, ActivateNativeModel, StopNativeModel}
  -> NativeModelWorkflow (one active process; desktop runtime owns its lifetime)
     -> NativeRevisionStore (sole durable selection pointer, immutable package revisions)
        -> NativeModelRegistry (manifest, SHA-256 and backend-scoped runtime admission)
     -> NativeModelController (active package, lifecycle)
        -> NativeModelProcess (long-lived native EXE, versioned IPC, Ready only from worker)
  -> NativeWorkflowSnapshot
     -> NativeRevisionPresenter (metadata inventory; `NotChecked` and `NotObserved`)
     -> NativeModelPresenter (last observed worker state)
     -> NativeHubPresenter (pure match of selected revision vs active process)
  -> NativeRevisionRow (ephemeral Slint visual data only)
```

No extra event bus, capability framework, persisted GUI catalog, or duplicate native selection in `AppSettings`. Existing Python AI Hub install preview and model entries remain isolated; native commands cannot invoke its overlay/wheel installer. The Native tab derives its rows from Core's manifest inventory. It displays one row per declared backend of each revision; it does not infer a service category from the model ID.

## Data directories and activation

The desktop runtime reads existing managed roots:

- `AIData/NativeModels/revisions`: `objects/<approved-manifest-SHA>/<model-id>/native-model.json` and authoritative `selections/<model-id>.json`.
- `AIData/NativeRuntime`: selected-backend runtime files shared between compatible packages.

These directories are **not created by UI refresh**. When both are absent, the native list is empty; when only one exists or the layout is invalid, Core reports an error. They must be provisioned by the trusted native package installer. The UI never accepts an arbitrary EXE path, installs packages, or infers external manifest approval from its own digest.

`NativeModelWorkflow::select(model_id, manifest_digest, backend)` performs the existing full registry/content verification and commits a single Core-owned atomic selection pointer. `activate_selected(model_id, deadline)` resolves that exact pointer, rechecks all files and backend compatibility, launches one child and waits for `load`, `warmup`, then `status` to confirm `Ready`. A second active session is rejected; it is not auto-swapped when a selection changes. `deactivate()` unloads/stops, and worker status is observed periodically through IPC. The desktop runtime reaps the process on shutdown; `NativeModelProcess` also has kill-on-drop.

The Native tab deliberately describes worker phases as **the last IPC observation**. `NativeRevisionPresenter` metadata does not certify executable or weight integrity. Only a live worker can confirm inference-ready. The desktop runtime publishes a bounded, sanitized error message for failed actions rather than raw worker output or user secrets. No automatic fallback on old versions or CPU backends.

## Tests and nonclaims

The `native_revisions` integration target exercises the full workflow using the existing synthetic Rust protocol fixture:

- no implicit selection/activation;
- same-PID inference after switching the durable pointer, followed by explicit shutdown and new activation;
- unapproved manifest, modified weights, and correct metadata-only inventory;
- worker crash revokes readiness without rewriting selection.

These are process/protocol tests, **not genuine audio inference**. The native read model is currently refreshed through explicit commands and a five-second worker poll. Verified published model packages and trusted source-signature verification remain separate unfinished gates. Also outstanding: native HTTPS/resume, Windows process-tree and external CUDA DLL validation, audio transfer schema, cross-process lease-safe garbage collection, real model EXE evidence and GUI interaction E2E on Windows. No old model revisions are deleted.
