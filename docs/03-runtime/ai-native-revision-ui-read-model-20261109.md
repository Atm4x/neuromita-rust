# Native revision inventory: lightweight Core → presenter boundary

## Source of truth

`NativeRevisionStore` remains the sole application-owned owner of published native revision manifests and explicit selection pointers. This slice does **not** add native model selection to `AppSettings`: mirroring the choice there would introduce two conflicting mutable owners. `NativeRevisionPresenter` maps Core data into typed, read-only GUI values; it must not discover models independently, choose a default, or issue install/load commands.

The existing `NativeModelPresenter` separately maps a **live model process** snapshot. A revision manifest or selected digest is never a live worker status.

## Two deliberately different read paths

- `overview()` enumerates at most 256 SHA directories, parses bounded native manifests and selection JSON (maximum 512 bytes per selection), and returns a sorted inventory. It checks manifest syntax, SHA-to-directory identity, path safety, selection schema/ID and declared backend compatibility. It **does not hash the executable, weights, or external runtime assets**. A selection referencing a missing manifest or undeclared backend appears explicitly as `Missing` or `BackendUndeclared`; no fallback is attempted. Malformed JSON, links and unexpected selection entries fail closed.
- `selected()` retains the strict full content verification (`prepare_launch`) before it may be used by `controller_for_selected()`. The latter rechecks and pins a specific revision before activation. Real `Ready` is established only through worker `load → warmup → status` over IPC.

This split avoids repeatedly reading multi-GiB model weights during AI Hub refreshes without weakening admission to execution.

## Presentation contract

`NativeRevisionPresenter::read(store)` exposes declared model ID, display name, package version, manifest SHA, available backends, and the explicit selected digest/backend. Both package and selection views mark `artifact_verification = NotChecked` and `worker_observation = NotObserved`, **not** `Installed`, `Verified`, or `Ready`.

A GUI must combine this projection with independently obtained verified installer / live worker snapshots only if it labels the separate sources and time of observation. No Slint screen is changed in this PR.

## Known limits

The overview is not a transactional snapshot across simultaneous external installation and selection writes; it fails closed on transient malformed/incomplete reads. It does not authenticate a publisher or protect against an attacker who can mutate managed directories while the application is reading them. It also does not validate package bytes. User-facing progress or selection mutation remains a follow-up composition task; the existing selection file remains the authoritative choice until then.

Agent C is strictly the NeuroMita integration owner: independent Rust model implementations, RVC/Fish Speech and neural math belong to another agent.
