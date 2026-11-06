# C2 — nonblocking AI worker health observation

Scope: Rust-owned `AiSupervisor::refresh_status`; partial progress on issue #24.

## Failure and observable result

In split topology, a slow TTS IPC request holds its `WorkerProcess` mutex. Previously, health polling waited on that mutex before checking the other worker slots. A simultaneous ASR child crash could therefore leave the presenter waiting until TTS finished or timed out, despite ASR having an independent process.

`refresh_status` now reads each process with `try_lock`. A busy worker is skipped during that poll; independent unlocked workers are still checked. This is a liveness observation, not a readiness probe: the skipped worker retains its last known state until the next poll or IPC failure. Worker generations still scope failure updates, so an old failed worker cannot overwrite the state of a replacement.

When a failure has already been observed, a subsequent health poll preserves its first diagnostic reason and emits no duplicate `watch` notification. An aborted in-flight IPC transaction retires the worker; the next unlocked health poll reports `Failed` and removes its active-model status. Explicit `stop`/`start` remains required for recovery; this does not introduce automatic retries or a second process supervisor.

## Test evidence

- `cargo test --locked --offline --lib`: 350 passed / 0 failed, using user-provided Rust 1.98.1 and existing vendor.
- `cargo test --locked --offline --test ai_runtime_cancellation`: 1 passed / 0 failed; observes `Failed` after cancellation and a new generation with `ModelReady` following explicit restart.
- `cargo test --locked --offline --test ai_runtime_composition`: 1 passed / 0 failed.
- `PYTHONPATH=python python3 -m unittest discover -s python/tests -p 'test_*py' -q`: 99 passed / 0 failed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

ALSA linking in the Linux test container uses temporary pkg-config metadata and a symlink to its preinstalled `libasound.so.2`, outside this repository. No Cargo manifest, lockfile, vendor or system packages were modified. The complete integration suite is not declared green: some fixtures require unavailable Python 3.12. Separate Clippy results are reported in PR metadata.

## Remaining C2 acceptance

- Explicit shared/split restart policy and crash exhaustion/circuit behavior.
- Late response and correlation under worker replacement, timeout and cancellation across all operations.
- Readiness separation for installed environment, running worker and loaded model.
- Actual user-facing recovery/selection behavior with recorded Python parity.

Do not close #24 on this health-poll change alone.
