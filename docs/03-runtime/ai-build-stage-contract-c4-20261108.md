# C4: declarative build-stage contract (Fish Speech / Triton)

Status: **metadata and strict admission guard only**. This is not a Triton compiler, not a compiled kernel receipt, and not a working weight installation flow. The production installer refuses plans with nonempty `build_stages` before starting backend/overlay publication; `PluginInstallHandler` separately refuses any direct install of such a plan. Existing overlay-only builtin remote API installs remain permitted and unchanged.

## Authority and wire contract

`neuromita_ai_sdk.BuildStage` is declared by each Python plugin through `InstallPlan.build_stages`. Rust deserializes the same `PluginInstallPlan` field with typed, strict `BuildStageKind` and `BuildPlatform`. Old plans omit it and get `[]` through `serde(default)`. No new SDK version, crate, storage schema, SQL table, event bus, or UI-owned installation state is introduced.

A stage specifies a unique safe ID, a method (`python_wheel`, `source_build`, `runtime_jit`), one backend already declared by the same plan, target platform, and an optional package requirement. A wheel stage **must** refer to an existing declared requirement and that requirement must be valid. Other stages cannot smuggle arbitrary pip/shell arguments in this field. Invalid kinds, unknown keys, duplicate IDs, unlisted backends, or mismatched requirements fail before any overlay installation.

Stages are included in the install-plan review SHA-256 fingerprint. `InstallPlanReview` exposes their presence to UI and rejects automatic `overlay_only` eligibility when stages are present. The UI displays the count and the statement that stage execution is unsupported; it does not claim compiled readiness.

## Fish Speech CUDA policy (source-based)

`neuromita_voice.plans` now selects platform-appropriate metadata based on the plugin's **validated settings** and selected backend. CPU Fish Speech has no Triton stages. On Windows, Fish Speech with CUDA declares `triton-windows<3.4` as an install requirement **and** a separate `python_wheel` stage, followed by a `runtime_jit` stage. On Linux with CUDA, it declares a `runtime_jit` stage but does not erroneously install `triton-windows`. This does **not** pin a Linux Triton wheel: compatibility with PyTorch/CUDA and exact OS/driver versions must be resolved from an approved backend lock, not guessed here. The JIT stage records expected runtime compilation but does not perform it.

## Next execution gates

1. Resolve concrete backend, locked packages, target platform and Python minor; compare against actual hardware and driver.
2. Resolve and pin wheel source, exact version and SHA-256 (or signed trusted build recipe/inputs and toolchain for source builds), and stage changes in the portable AIData location only.
3. Execute in a supervised process with timeouts, bounded logs, cancellation, operation ID, and atomic verified receipts; never execute arbitrary instructions supplied by an untrusted community plugin.
4. For runtime JIT, run a real GPU smoke kernel with the selected model/backend and record an evidence-bound result. Package presence alone is not `ModelReady`.
5. Expose approved preflight, granular operation updates, restart/recovery and final verification through one existing Core-owned presenter. Do not add another modal for each compiler step.

Tests: `PYTHONPATH=python python3 -m unittest python.tests.test_build_stages -v`; Rust `ai_runtime::install::tests` and `ai_runtime::install_review::tests`. Live Python SDK test verifies that the real builtin Fish plugin emits the typed stage; this does not require a GPU and is **not** a kernel compilation test.
