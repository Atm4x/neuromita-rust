# AI Installables — removal reference preflight (C3)

Current status: **advisory preflight**, not filesystem removal. There is deliberately no `remove()` or `uninstall()` API in this slice.

## Authoritative references available today

- `ServiceSelection` is the application's configured backend/overlay association for each AI service. A shared backend can be selected by several distinct services, and removing one overlay does not release another service's backend reference.
- `WorkerProcess` now retains the exact package-layer search paths used to launch its Python process. Those paths remain tracked until the worker is stopped and reaped, even if health changes to `Failed`. They are not reconstructed from a user-editable status label.
- `AiSupervisor::observed_worker_layers` reads both activation and worker guards without waiting for a long IPC request. If a worker is busy or an activation is pending, it returns `LayerUsageError::Busy`, **never an empty list**. This is intentionally fail-closed for preflight.
- `removal::assess_backend` and `assess_overlay` compare exact installed plan identities with configured selections and exact installed paths with worker generations. The returned `RemovalAssessment` shows explicit `SelectedBy(service)` and `ActiveGeneration(uuid)` blockers. The API does **not** silently treat an unselected-but-running revision as unused.

## Why no deletion yet

A point-in-time snapshot cannot prove there is no concurrent worker activation **after the snapshot**, nor can it establish references from another application instance. The set of `ServiceSelection` supplied by an application must also be complete; this function cannot discover every configuration by itself. Consequently, an assessment with no observed blockers **is not an authorization token** and must not be wired directly to `remove_dir_all` or a GUI delete button.

A safe destructive operation requires a single application-owned operation holding lifecycle exclusion for the entire delete, a consistent inventory of saved selections/active workers, and cross-process publication/worker acquisition coordination. The backend shared-dependency contract must explicitly refuse deleting a still-referenced backend. Failure of any proof retains the immutable installed revision.

The existing installer and worker APIs remain unchanged. This module is read-only and adds no persisted schema, PID lease, fallback, dependency, or root selection. It never edits Legacy code or imports settings ownership into the Python SDK.

## Canonical path identity

`RuntimeComposition` canonicalizes each installed layer before passing it to a Python worker. A raw install path may refer to the same directory through an alias in an injected ancestor. Preflight therefore compares the filesystem identity of existing paths using `canonicalize` for both saved selections and active worker imports. A UNIX regression covers an alias at the configured backend root with a worker holding the canonical path. Missing paths are left lexical for read-only reporting, **never promoted to deletion authorization**; future destructive removal must require a present, revalidated immutable receipt and exclusive lease.
