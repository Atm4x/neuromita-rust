# C3 — immutable package revision lifecycle leases

## Contract

The installer, receipt readers, and Python worker launch now participate in one OS file-lock protocol for each installed **backend or overlay revision**:

- Deterministic sibling marker `.neuromita-revision-<revision>.lock`, with a zero-length regular-file invariant. The managed parent is canonicalized before opening it, and Unix file identity is checked after opening; symlink markers are rejected.
- Every Python worker holds an OS **shared** lock on each actual import-layer revision from **before spawn** until the `WorkerProcess` is dropped, including during slow `initialize`, IPC and a failed generation that remains tracked. One-shot inventory workers use the same launch path. Layers are acquired in sorted order. If any required lease cannot be acquired or its revision has disappeared, the worker is not spawned.
- Backend and overlay receipt readers (including `LayerInstaller::installed`) obtain the same shared lock for their entire inspection. Existing symlink/receipt checks remain active.
- UV layer publication obtains an **exclusive** lease before the immutable destination's final verification and synchronous rename. Contention is retried asynchronously within the configured installer timeout. It cannot rename an unpublished revision during an active layer reader holding its lease.
- The marker file is **never unlinked**, including after the last lease holder exits. Deleting it would allow different processes to lock different inode identities for the same revision.
- On lock contention, readers and worker launches **fail closed** with a lock/IO error rather than reporting the layer absent or starting from unverified files.

## Tests

Rust unit tests cover simultaneous shared readers versus an exclusive mutation owner, real cross-process shared ownership through a separate test executable, symlink-marker rejection, installed receipt readers refusing an exclusive owner, and a real launched Python worker holding the shared lease until its generation is stopped and reaped. Existing installer/concurrency tests remain enabled.

## Boundary of acceptance

This provides OS-coordinated revision leases while the **owning Rust process is alive**. It is not yet authority to physically uninstall. A Python worker that survives an abrupt owner crash can outlive the owner's shared lock; this must be solved per OS before enabled delete. A configured selection in another process need not have started a worker. Also, no complete configured-selection transaction or cross-process deletion/quarantine recovery protocol has been implemented. The existing `removal::assess_*` functions remain read-only. **No installed revision is deleted by this change.**

The protocol assumes an application-owned, local filesystem. It does not promise adversarial symlink/TOCTOU security or remote-filesystem lock coherence. Shared lock behavior on Windows requires Windows CI acceptance, not Linux inference.
