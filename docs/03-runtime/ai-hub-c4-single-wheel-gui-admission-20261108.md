# AI Hub C4 — single-wheel install action

## Ownership

The same AI Hub page can confirm one of two Core-approved operations. A
`remote-api` plugin with no dependencies or model assets can publish its plugin
*overlay*. A builtin plugin with exactly one `python_wheel` build stage can
publish that stage's immutable **wheel layer**. The second action is not an
installation or activation of the complete backend/model.

`ai_runtime::install_admission::preview_install_action` performs Core-side
review and returns an `InstallActionReview` containing a private,
Core-selected action. The GUI sends only the selected speech identity and a
review token. It cannot supply packages, wheel hashes, paths, commands, stage
IDs or backend strings. `InstallApprovalGate` binds the token, identity,
review and action, and consumes the approval once. A new preview or catalog
refresh invalidates it.

Only one executable wheel stage is offered, when:

- The source is a verified builtin plugin, the backend is unambiguous, and
  the Python plan contains exactly one `python_wheel` stage and one matching
  package requirement. Plans with model assets, additional backends, plugin
  packages, custom actions, hooks, or other build stages are not admitted.
- The builtin's reviewed `wheel-stages.lock.json` identifies exactly one
  matching stage and exact `name==version`, platform and SHA-256 wheel pin.
- The wheel is already present under the portable `AIData/cache/wheels` tree,
  with a matching checksum; the installed Python and uv binaries have matching
  checksums. This preflight performs no package installation or download.

On confirmation, `install_reviewed_wheel_stage` fetches and validates a fresh
Python plan, compares the reviewed source/fingerprint and delegates to the
existing `WheelStageInstaller`. The installer repeats all checks and invokes
uv against an operation-owned verified wheel snapshot before atomically
publishing a single immutable build-stage layer.

The UI renders `wheel-review`, `installing`, `wheel-installed` and failure
states separately from overlay installation, runtime `ModelReady`, and model
asset verification. An in-flight operation remains visible across catalog
refresh/navigation under its real Core-assigned operation ID. An interrupted
Core session is reported as unknown, not successful.

## Explicitly excluded

There is no package download from URLs, arbitrary command runner, source
build, Triton kernel JIT, multi-stage dependency orchestration or
backend/model activation. The Python `installplan.py` execution is not an OS
sandbox and is deliberately limited to installed trusted sources. Dynamic
user settings are not passed to this review path yet. A worker/model may only
be marked ready from a separate Core runtime signal. This path does not
provide a crash-durable operation queue or restart reconciliation.

## Acceptance gates

Library tests cover a missing wheel, checksum mismatch, exact pinned action,
source/lock mutation, invalid backend and one-use approval. `cargo check
--features gui --bin neuromita-gui` checks the full Slint callback and Core
command integration. The real uv wheel publication is independently covered
by the existing `ai_runtime::build_stage::tests` suite. Neither a successful
GUI typecheck nor the local wheel fixture proves Fish Speech GPU inference.
