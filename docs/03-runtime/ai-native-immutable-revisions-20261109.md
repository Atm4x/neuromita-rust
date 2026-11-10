# Agent C — immutable native model revisions and explicit selection

**Scope:** Integration of independent native EXE packages supplied by the model agent. No RVC/Fish Speech source, model weights, inference math, Python/uv installation or GUI components are changed.

## Authoritative layout

```text
NativeRevisionStore root (application-owned)
  objects/
    <lowercase manifest SHA-256>/
      <model ID>/native-model.json + executable + weights
    .<digest>.native-staging/ ... installer ownership lock and transient UUID stages
  selections/
    <model ID>.json            explicit choice: schema version, model ID, digest, backend
    .install-owner.lock       stable OS advisory selection ownership

External shared native runtime root
  <declared relative backend runtime paths>  immutable create-if-absent verified blobs
```

The **manifest and verified files** remain the only catalog of published packages. `objects/<SHA>` is a revision *namespace*, not a second mutable package database. The `selections/<id>.json` record is a *user/application decision*; it is neither an installation receipt nor evidence that the EXE is loaded, warm, or ready. No implicit newest-version resolution, semver selection or backend fallback exists. Both the version and backend must be chosen explicitly.

`NativeRevisionStore::install_local` creates a SHA-addressed package root, then reuses the existing `NativePackageInstaller` publication and verification behavior. The underlying installer still owns safe immutable local staging, shared runtime checks and atomic package directory rename. A failed/incomplete revision never appears in the registry; empty revision namespaces may remain for crash retry. Source approval must originate independently from a trusted distribution decision, not from hashing untrusted download contents.

`NativeRevisionStore::list` enumerates published manifests and verifies that the directory SHA is the actual manifest digest. It does **not** claim that every backend declared in a manifest is installed, that the model has loaded or that inference succeeds. `select` first performs the full runtime/content admission check, then writes a bounded, strictly typed selection snapshot to a temporary regular file and fsyncs it. With the stable selection OS lock held, an atomic rename replaces the previous choice, followed by parent-directory sync. A crash before rename leaves the old selection. A crash after rename exposes the new complete selection. UUID-named abandoned selection temporary files can be removed only by the next exclusive owner; unknown entries and unsafe symlinks are never silently removed.

`selected` reads the chosen digest strictly, verifies the published package and backend-required runtime blobs, and returns an explicit typed choice or an error. If absent, it returns `None`, **not** the latest revision. A corrupted selection, altered executable, weights or shared runtime, or a missing requested version fails closed; no automatic rollback occurs. The `NativeModelController` returned by `controller_for_selected` owns its immutable version root for the duration of its process session. Changing `selections/<id>.json` does not mutate or redirect that controller. The existing `NativeModelPresenter` continues to project only live worker status.

## Crash safety and residency

Multiple completed revisions coexist. **There is deliberately no deletion/GC API**. An in-flight inference process can continue using its original package directory during a selection change, or even when another instance starts a newer revision. OS locks serialize participating writers to the selection record and participating installers to package/runtime staging. They do not sandbox arbitrary OS writers; filesystem mutation by another actor with write access remains an acknowledged TOCTOU threat. A future garbage collector requires an independent cross-process lease/epoch protocol before any revision can be removed. Filesystem write permissions, publisher signatures, Windows replace semantics, process-tree lifetime and actual CUDA DLL loading require target-platform verification.

## Verification gates

Test upgrading and rolling back between two different manifest hashes with the **same model ID**, two concurrently resident independent worker processes, same PID and increasing sequence on the old worker after selection changes, independent new-worker sequence, read-only selection inventory, exact idempotency, no implicit fallback, corrupt/torn selection rejection, changed weights/runtime rejection, symlinked temporary file fail-closed, lock contention, and OS SIGKILL of the selection owner followed by orphan reclamation without touching the previously published choice.

Existing native installer, registry, controller and process integration suites must remain green. Fixture workers and synthetic weights are **not RVC model inference**. This cycle does not implement network distribution, cryptographic publisher identity, multi-revision garbage collection, Windows process-tree cleanup or production GUI integration.
