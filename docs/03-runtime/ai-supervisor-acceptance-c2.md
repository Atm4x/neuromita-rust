# C2 supervisor acceptance — 2026-11-07

Scope: Rust-owned Python host, lifecycle, correlated JSON-lines IPC, shared/split services, persisted recovery. The tests use real OS Python processes with deterministic fake inference; they do not prove model weights, drivers, commercial APIs or production voice quality. This is the Rust supervisor contract, not a certification of every historical Python application's user-visible workflow.

| Requirement (#24) | Executable evidence | Observed contract |
| --- | --- | --- |
| Startup, handshake, API version, plugin discovery | `ai_runtime::tests`, `ai_runtime_composition`, `ai_application_lifecycle` | Child process owns a generation; a valid hello means `WorkerRunning`, not `ModelReady`. Invalid host/locked Python is not silently bypassed. |
| Real model initialization and readiness | `ai_hardware_acceptance`, `ai_supervisor_recovery`, `ai_runtime::tests` | Only `model_ready=true` for the selected plugin may publish `ModelReady`. Hardware selection additionally requires device execution and runtime echo. |
| Correlation, IPC failure, unknown component, stale replies | `ai_runtime::tests`, `ai_runtime_cancellation` | Bad frames, EOF, crash, timeout and cancelled request retire the generation; stale generation cannot mark replacement ready. A plugin-level error is distinct from transport failure. |
| Shared/split failure isolation and restart | `ai_supervisor_recovery::{shared_recovery_*,split_recovery_*}` | Shared crash invalidates both services; split TTS crash/recovery keeps ASR worker generation and readiness. |
| Atomic active TTS switch | `split_tts_model_switch_is_atomic_and_does_not_restart_healthy_asr` | Invalid candidate cannot replace ready TTS; valid replacement produces a new TTS generation, while ASR continues inference on its original process. |
| Cancellation of inflight inference and initialization | `ai_runtime_cancellation`, `cancel_inflight_python_initialize_keeps_failed_generation_retryable`, `live_speech::tests` | Cancelled request cannot return late response from an older worker; failed-generation recovery remains retryable. |
| Installed settings and bounded recovery | `installed_speech_watcher_recovers_mock_worker_from_saved_settings`, `recovery_policy_caps_attempts_and_enforces_real_backoff_per_generation` | Recovery plan is recomputed from observed persisted speech selection, with explicit retry limit, poll interval and backoff; exhaustion remains visible through `RecoveryAttempts` and the GUI status renderer. |
| Hardware-profiled crash recovery | `hardware_worker_crash_recovery_requires_same_persisted_profile_and_reproves_execution` | A crashed hardware-selected worker is not software-downgraded. Explicit retry checks pinned profile, persisted revision, device availability, execution and model readiness before replacing the failed generation. |

## Implementation scope

`HardwareRuntime::recover_failed` provides an explicit retry entrypoint for hardware-profiled processes. `RunningAiApplication::recover_failed_hardware_worker` validates complete service selection, interpreter and process topology. `AiSupervisor::recover_failed_hardware_guarded` accepts only the failed generation and exact prior runtime profile, then re-runs hardware probes and model initialization on a fresh Python child. A mismatched profile or changed selection is rejected before publication.

`RecoveryPolicy::new(poll_interval, retry_backoff, max_attempts)` configures bounded automatic retries for the live speech watcher; `LiveSpeechRuntime::new_with_recovery_policy` applies it without introducing persistent schema, a new supervisor, an event bus or implicit retry of inference. Default behavior retains the previous 3-second poll and 2-attempt cap. No automatic hardware fallback is introduced; hardware recovery is an explicit operation.

## Remaining external verification, distinct from Rust C2 acceptance

- Compare actual active Python NeuroMita source revision, supported third-party AI operations, and OS/device-specific behavior; historical audit documents are not evidence of current runtime reachability.
- Managed Python 3.12 installed via real uv bootstrap, real CUDA/DirectML drivers, actual weights and production audio remain external integration acceptance. The Linux test runtime uses Python 3.13 mock processes.
- Existing issue #24 and release acceptance should be closed only after the above cross-repository parity review and after these stacked commits are integrated; this document does not silently change issue state.
