# Native package installation boundary — Agent C

**Scope:** NeuroMita Core integration for independently built native model executables. This is **not** a Rust model implementation, an inference backend, a downloader, or a GUI workflow.

## Ownership

```text
Trusted external package approval (manifest SHA-256, selected model ID/backend)
  -> NativePackageInstaller::install_local
      -> NativeModelRegistry::discover and prepare_launch (source admissibility)
      -> package/runtime OS install locks (exclusive, persistent inode)
      -> recover abandoned UUID staging after previous owner died
      -> verify and stage EXE + assets (streaming file-size/SHA-256 checks)
      -> verify and stage selected backend's native runtime files
      -> publish compatible runtime blobs without overwriting any existing path
      -> atomically rename the completed model package directory into place
      -> NativeModelRegistry::prepare_launch on published content
  -> NativeModelController::activate (separate operation)
      -> live worker capabilities/load/warmup/status -> Ready -> infer*
  -> NativeModelPresenter::project (only worker-reported readiness)
```

`NativePackageInstaller` owns **neither** the model implementation nor a new global model catalog. The pre-existing `NativeModelRegistry` remains authoritative for model package discovery; the registered directory is never visible until its files have passed verification and the runtime dependencies have been committed. The application must keep the approval digest in a trusted catalog or distribution decision external to the untrusted source directory. A SHA-256 read from the same untrusted manifest is **not** authentication.

The installer accepts local source package/runtime roots, a model ID, an externally approved SHA-256 manifest digest, and the selected backend. It does not invoke model executables. It copies only files listed in the approved manifest, and only runtime dependencies declared for the selected backend. A CPU install of a CPU+CUDA package does not require CUDA-only libraries. Shared runtime names are pinned to exact bytes: equal verified content is reused; conflicting content is a hard error; nothing silently replaces another model's runtime. Runtime installation uses same-filesystem hard links from private, fsynced staging to provide create-if-absent visibility. The executable retains safe owner-executable Unix mode. The fully verified package is published by a same-filesystem directory rename, last.

Crash recovery operates only within installer-owned sibling staging roots (e.g. `.<packages-root-name>.native-staging`), after acquiring advisory OS ownership locks in stable marker files. A killed owner releases the OS lock, so the next operation can remove stranded UUID staging directories. An active install holding the lock is never reclaimed. Unrecognized foreign entries are left untouched; a UUID entry with an unsafe type causes a fail-closed error. Once a runtime file is published, it can be reused even if a process dies before package publication. A package that already exists with the **same** approved manifest is verified and reused; an attempt to replace it with another manifest is denied and the prior installation remains readable.

## Deliberately unimplemented gates

- **No multi-revision activation:** the current registry supports one immutable *published slot* per model ID. A different manifest/version is rejected, not upgraded or overwritten. Versioned side-by-side installs, selection and garbage collection are separate changes.
- **No HTTPS transport or signature chain:** source downloads, remote trust policy and publisher signatures must precede this local install boundary. The caller cannot self-approve an untrusted manifest.
- **No OS sandbox or no-TOCTOU guarantee:** OS-level lock files coordinate participating installers, not arbitrary adversarial processes with write access to managed roots. A privileged external actor could mutate a published binary. Worker launch revalidates hashes, but immutable filesystem permissions/code signing and race-free handle-based launch are separate hardening tasks.
- **No all-or-nothing multi-runtime transaction:** shared runtime files are atomically created one by one. If a later step fails or a process dies, already validated shared blobs remain installed; no partially built model package becomes discoverable. Garbage collection and model/package rollback require a separate owner.
- **Windows/driver matrix unverified:** atomic rename/hard-link semantics and runtime DLL loading must be verified on supported Windows filesystems. No global `PATH` changes and no CUDA Toolkit requirement are introduced by this code. NVIDIA's GPU driver remains a system prerequisite when CUDA is selected.
- **No real inference:** executable fixture is an independent Rust test process, not RVC/Fish Speech. Numerical parity, weights, actual sound inference and their dependencies are owned by the separate model-development agent.

## Validation requirements

Use Rust 1.98.1 and vendored Cargo offline. Integration tests should cover end-to-end install followed by activation and two `infer` calls with the same PID, idempotent retry, rejected altered source/unauthorized digest, shared runtime conflicts and reuse, CPU without CUDA runtime, orphan recovery, OS lock contention and real child kill, and symlink rejection. Never infer model `Ready` from installation receipts; only the running model process may confirm it.
