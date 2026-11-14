# Issue #417 — shared installation ownership, phase 2

Base: `1994c5650d123c944f0fd62ce3985486fbd56a07` (draft PR #418). Contract: `future/model-module-installation-contract.md`.

## Implemented integration

`ApplicationOwner` now creates **one** `InstallationOwner` for the AI data root and passes the identical `Arc` to `AiHubInstallCoordinator` and `AiHubNativeService`. Both executors must acquire one global installation admission **before executing** an approved installation. This replaces their separate admission decisions, not the underlying verified Python/native component executors.

- `InstallationTarget` records the generic component kind (host/package/bundle/runtime/resource), service when known, module/package ID, exact declared revision, and component type. Revisions are checked as bounded hexadecimal digests. It is an installation identity, not a loaded-model session nor a hardware compatibility decision.
- `InstallationPermit` uniquely owns an active operation, cancellation token, and existing `InstallOperation` journal. The journal begins **before** work and marks verified/completed **only after** the specialized installer returned success. Failure and interrupted/cancelled tasks cannot assert readiness. The committed operation record is durable; a crash in a nonrepeatable stage requires explicit receipt reconciliation.
- `InstallationOwner::subscribe` exposes one observable `InstallationStatus` to future GUI/TUI projections. It reports executing, cancellation requested, cancelled, completed, failed, interrupted. Runtime operations still emit their original detailed download/preflight progress events.
- Python overlay/wheel execution is bounded by the owner's cancellation token, and the worker task is joined/aborted by the existing coordinator. Its subprocess executor retains `kill_on_drop` protections.
- Native HTTPS transfer shares the same cancellation token and **exact operation UUID** across its progress events, managed task and durable journal. The journal's final result comes from the joined installer task, **never** from a caller-provided GUI success flag. The native service waits for the task to release its staging owner before dropping the installation permit.
- Application shutdown closes new installation admission, signals cancellation, then awaits the existing executor shutdowns. No independent third installer process or second background owner was added.

## Ownership and call tree

```text
ApplicationOwner
  InstallationOwner [one shared Arc, one admission, one operation journal]
     |-- AiHubInstallCoordinator [Python review, executor task, UI projection]
     |     \-- verified overlay / pinned wheel component executor
     `-- AiHubNativeService [trusted release selection, executor task, UI projection]
           \-- NativeDownloadOperation -> HTTPS transfer and package publisher
```

## Deliberate boundaries

This stage does **not** claim full #417 completion: bootstrap host installation and dynamic module-owned `InstallPlan` after bootstrap still need their own typed orchestration. Existing preflight cannot run without the current Python SDK host; native pre-install module declarations are not yet signed independently of inference packages. `InstallationTarget` currently does not carry a selected physical Hardware/ExecutionConfiguration or module-plan digest; existing component-specific checks still apply. A single active installation globally is a conservative admission policy pending stage-aware scheduling. The legacy specialized tasks remain responsible for their own subprocess and staging cleanup. No model readiness or automatic fallback is inferred from package publication.

## Acceptance checks

The shared-owner unit tests cover cross-implementation admission exclusivity, immutable operation ID and durable inspection, cancellation and shutdown, and failure/aborted operation not falsely recording completion. Existing native and Python executor integration tests must still pass. Full clean-profile host bootstrap, hardware binding, per-service atomic model reconfiguration and a unified typed presenter remain open acceptance gates.
