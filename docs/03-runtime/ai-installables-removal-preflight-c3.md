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

## Root→revision exclusive inspection boundary (C3.3)

`removal::inspect_backend_with_root` and `inspect_overlay_with_root` are internal,
read-only preparation operations, not a public uninstall API. An already acquired
`SettingsInstanceLease` for the portable data root is required **before** an
exclusive `RevisionLease` is acquired. The installed layer must live at the
exact `AIData/backends/<kind>/<revision>` or
`AIData/overlays/<plugin>/<revision>` location; the inspected directory and
managed ancestors must exist as real (non-symlink) directories. The pinned
immutable receipt must be present, non-symlinked, bounded, parsable and equal
exactly to the requested plan. Any mismatch, missing directory, unsafe path or
contending worker/installer OS lease fails closed.

The returned internal `LeasedRemovalInspection` keeps both the root owner
borrow and exclusive revision lock alive while its `RemovalAssessment` is used.
A second cooperating Python import cannot acquire a shared revision lock until
that inspection is released. Existing configured-service and observed-worker
blockers are still reported instead of discarded; no blockers is **not** a
permission to rename, quarantine or physically delete. Callers can supply an
incomplete `ServiceSelection` list, in-process settings mutations are not yet
frozen and custom repositories can be outside this contract. A future remover
must first establish a complete authoritative persisted selection snapshot,
freeze in-process activation and settings changes, and define an atomic,
crash-recoverable quarantine transaction under these same locks. This slice
contains no destructive operation and no GUI Remove.

## Persisted settings maintenance freeze (C3.4)

`SettingsOwner::freeze_persisted` obtains an exclusive in-process maintenance
state under the existing settings mutex, only when its current revision is
reported durably saved. While the RAII guard remains live, all `edit`,
`replace`, `import` and explicit `request_save` operations return
`SettingsError::MaintenanceActive`; reads retain the stable snapshot. A pending
or failed save is **not** a maintenance snapshot. Dropping the guard restores
normal edits, which immediately invalidate the prior persisted proof until the
new revision is saved. Existing API callers receive the settings-conflict error
code rather than silently succeeding.

`freeze_persisted_for_root` additionally requires an application-owned settings
repository bound to the same canonical root as the portable
`SettingsInstanceLease`. In-memory or ad-hoc repositories cannot impersonate a
complete persisted selection inventory. `inspect_backend_with_frozen_settings`
and `inspect_overlay_with_frozen_settings` combine this freeze with the earlier
root→exclusive revision lock; the returned RAII assessment holds **both**
until drop. Tests cover concurrent settings writers, independent repository
rejection, pending revisions, exclusive revision locks and re-acquisition after
drop.

This is still strictly **read-only and advisory**. `ServiceSelection` remains
supplied by the caller and is **not yet guaranteed complete**; the persisted
speech settings record plugin choices rather than exact locked backend and
overlay plans, so a separate verified manifest resolver is required. Activation
coordination outside cooperating revision leases remains unproven. No token
from this API authorizes rename, quarantine, physical delete or GUI Remove.

## Persisted speech overlay inventory (C3.5)

`InstalledSpeechEnvironment::persisted_speech_overlay_inventory` resolves the
currently selected local TTS and ASR plugins directly from typed persisted
`SpeechSettings`, the *actual* executable plugin manifests and the unique
installed immutable overlay receipt. It never executes plugin code, probes
Python or changes an installation. Any invalid discovery result, missing
selected plugin, ambiguous overlay revision or invalid receipt is an explicit
error rather than an empty set of references.

`removal::inspect_overlay_with_persisted_speech` holds the
application-owned persisted-settings freeze while resolving those selections,
then acquires the target exclusive revision lease and assesses exact selected
speech overlays. Its return value retains the settings, portable-root and
revision guards. Tests prove that an actually selected TTS overlay is reported
as `SelectedBy(Tts)` without trusting a caller-provided `configured: []`, and
that the result changes only after a settings edit has been saved and a new
freeze is acquired. Foreign portable roots are refused.

**Scope is deliberately narrow.** An overlay receipt does not attest which
backend revision an executable Python plugin requires. The inventory therefore
returns the explicit `unresolved_backend_services` collection for every
selected local speech service, and its `ServiceSelection.backends` is empty
only as an unresolved placeholder, **never** evidence that a backend is unused.
Non-speech services (Embedding/Reranker/Beats), foreign direct filesystem
writers, and crash-recoverable quarantine remain outside this proof. No
physical deletion or GUI Remove is enabled. A future backend inventory must
pin the exact installed plan and protect its production-to-activation binding
before it can be used by a destructive operation.
