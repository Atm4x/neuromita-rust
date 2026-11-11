# C3 — Windows installer child containment via a kernel Job Object

## Background and verified prior failure

The Unix inherited-stdin file-lock ownership established by PR #166 does not carry over to Windows: the Rust 1.98.1 Windows smoke in Actions run `42343170731` demonstrated that the parent file lock was released despite a surviving child with a duplicated handle. PR #170 therefore disabled orphan sweeping on Windows rather than deleting possibly live staging.

## Child lifecycle

For a Windows `uv pip install`, `LayerInstaller` now spawns the direct child and immediately assigns it to a private, **non-inheritable** Windows Job Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Job ownership belongs to the local `StagingCleanup`; a job is never a persistent asset or a second installation source of truth.

On a normal child exit, the resulting status and stdout/stderr are processed as before. On future cancellation or timeout, the cleanup task terminates the Job Object, polls `JobObjectBasicAccountingInformation.ActiveProcesses` until it reaches zero, and only **then** attempts to reclaim that staging under a fresh file lock. Failure to assign the child to the job, timeout while waiting for termination, or an OS query error keeps the staging directory rather than risking deletion of live installer output. The kill-on-last-handle policy also terminates assigned children when the application itself is forcibly terminated.

Windows **orphan sweeping stays disabled**: if the application dies before assignment, or after assignment without proving process termination to the next application instance, a stage is conservatively retained. The next design step is a durable kernel-attested ownership handoff or a supervised worker whose termination can be proven after restart. A new child could also run between `spawn` and `AssignProcessToJobObject`; failing closed on assignment errors and not sweeping orphan directories makes this race safe for files, but does not eliminate that process lifetime window.

## Acceptance tests

- Windows-only unit test: starts a real PowerShell child, assigns it to the job, terminates the job, waits for `ActiveProcesses == 0`, then reaps the child.
- Windows-only `LayerInstaller` integration-like unit test: starts a blocking `.cmd` uv fixture (which launches `ping`), aborts the install future after the child starts, then checks that staging is reclaimed and the immutable revision was never published.
- Existing Linux Rust library, real process crash tests, split-worker IPC and Python SDK regressions must remain green.

Windows tests **must run on an actual Windows runner** before this PR's Windows acceptance is claimed. The previous Windows inherited-lock test remains a known failure, not an expected PASS. Do not enable Windows orphan sweeping based on this PR alone.

## Scope

No new dependency crate: only additional features for the already pinned target-specific `windows-sys` version. No changes to public installation signatures, package locks, persistent root, assets, event architecture, model selection, or unrelated legacy scripts. Child process supervision covers the Job Object after successful assignment; Windows breakaway policies and nested job constraints must be explicitly surfaced as failures rather than silently downgraded. Issue #42 remains open for complete installation, removal, shared dependencies and assets parity.
