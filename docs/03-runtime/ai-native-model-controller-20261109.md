# Native model application ownership — Agent C

**Scope:** Core-side integration of existing independent native model executables, not implementation of RVC/Fish Speech or their numerical inference engines.

## Ownership and call graph

```text
Application composition (selects an approved package and backend)
  -> NativeModelController (one explicitly selected live model session)
       -> NativeModelRegistry::discover (read-only local manifest)
       -> NativeModelRegistration::prepare_launch (external digest approval,
          OS/architecture/backend, exact EXE/assets/backend runtime file checks)
       -> NativeModelProcess::start (pinned EXE, one IPC process)
          -> capabilities -> load -> warmup -> status (worker reports Ready)
          -> infer* (same process, no unload after each request)
          -> unload -> shutdown (explicit release)
       -> NativeModelSnapshot -> NativeModelPresenter (pure read model)
```

The application owns one `NativeModelController` **per desired model slot**. The controller does not own the full application or global catalog, and it does not select or fall back to another backend. A second activation is explicitly rejected until shutdown. This provides a narrow integration boundary for the separate agent producing native model EXEs. The model EXE remains responsible for tensor math, weights, backend implementation and audio data processing.

`NativeModelSelection` requires `model_id`, independently approved manifest SHA-256, backend and IPC timeout. **Discovery never automatically approves a manifest.** The caller must obtain digest approval from a trusted higher-level distribution decision, not derive trust by simply reading an untrusted model directory. No manifest signature verification, downloader or immutable installer has been added here. Existing Python installation remains untouched.

`activate` runs registry admission and completes the actual worker `capabilities -> load -> warmup -> status` cycle before accepting the session into the owner. Errors before acceptance terminate the local process via its existing kill-on-drop behavior; the session is not falsely published as Ready. `infer` reuses the same process and never implicitly unloads weights. `status` polls the actual worker while healthy; `last_observed_snapshot` is explicitly **not a heartbeat** and can be stale until a poll. A failed worker cannot continue inference. `deactivate` releases the selected model explicitly, and a subsequent `activate` is allowed only after that release.

The existing `NativeModelPresenter::project` receives only the process snapshot; `can_infer` is true exclusively for `NativePhase::Ready`. The existing Python `AiInstallOperationPresenter` is not modified, since installation ledger evidence is not evidence that a native model has loaded its weights or passed warmup. No Slint GUI or independent UI-owned readiness states are added.

## Acceptance and limits

The integration tests create a temporary registry, compile a minimal independent Rust protocol fixture, write a synthetic test-only weight file, and exercise registration to application owner to real OS child process. They test same-PID repeated inference, denial of unapproved manifests/unsupported backends, mutated weights, missing model, exclusive activation, explicit reactivation, crash and external SIGKILL. They are **not** actual RVC audio inference.

Not implemented: application startup binding/settings persistence, trusted native package distribution, atomic immutable installation and runtime receipts, Windows process tree kill/Job Object verification, native CUDA DLL loading, real audio/ONNX model input/output, in-flight cancellation, monitoring during an indefinitely idle worker, VRAM/RAM admission, multi-slot resource policy. Connecting the live GUI should wait for a real application-owned package approval and model session. The native model implementation remains the responsibility of another agent.
