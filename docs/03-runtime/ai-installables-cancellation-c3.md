# C3 — cancellation-safe Python layer staging

`LayerInstaller::publish` uses an immutable revision as its commit boundary. Python packages are first installed into a per-operation `.staging-<uuid>` directory under the managed backend or overlay parent; only a validated receipt and wheel metadata may be published.

Previously, cleanup ran after the awaited install future. Dropping that future while `uv` was running terminated its immediate subprocess (`kill_on_drop`), but skipped the cleanup branch and left a partial `.staging-*` directory on disk. An incomplete directory does not count as an installed revision, yet repeated cancellations could accumulate unbounded temporary data.

The installer now takes ownership of a newly created staging directory through a `StagingCleanup` drop guard. On normal failure or timeout the installer removes it before returning. On future cancellation the guard dispatches blocking filesystem cleanup independent of the cancelled future, without stalling the async executor on large directories. When no Tokio runtime is available, the guard removes the directory synchronously. Cleanup errors are logged, not silently ignored. The `uv` subprocess retains `kill_on_drop(true)`.

Staging directory creation and final rename use synchronous filesystem calls. There is no await between the atomic revision rename and disarming the cleanup guard: cancellation cannot leave a successful publication under an armed guard or cause an in-flight asynchronous rename to publish after cancellation. Already active immutable revisions are not mutated by staging cleanup.

The Unix mock-uv regression starts a real blocking installer process, aborts its owning Tokio task, waits for the cancelled staging directory to disappear, verifies the prior revision remains installed, checks timeout cleanup, and then verifies a successful retry publishes a new revision.

This is not a full crash-durable installation transaction: abrupt OS termination, forced process kill, orphaned grandchildren of third-party installer scripts, preexisting staging directories from earlier executions, concurrent publication to the same revision, and arbitrary user-controlled symlinks inside installed wheel payloads require separate recovery and integrity work. The managed root supplied by portable-root ownership remains trusted. Issue #42 stays open.
