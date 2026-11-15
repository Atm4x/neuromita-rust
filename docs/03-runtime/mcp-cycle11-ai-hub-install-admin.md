# MCP AI Hub Python installation: GUI parity (Cycle 11)

MCP/IPC is a peer administrative surface to GUI. These controls call the **same `AiHubInstallCoordinator`** instantiated by `ApplicationLifecycle` and consumed by desktop GUI. They do not create an additional installer or bypass installation admission, reviewed hashes, dependencies or the user-owned installation journal.

## MCP contract

Start MCP against a running application Core (`--core 127.0.0.1:<actual-port> --allow-write`). This procedure is privileged and may install software from a previously authenticated, locally reviewed manifest. Use only in a disposable testing profile or with deliberate administrator approval.

1. `inspect_presenter(surface=ai_hub_install)` returns a bounded snapshot: `status.revision`, `category`, `id`, `token`, `stage`, `can_install`, `action`, `operation_id`, `error_code`. It never exports script contents, raw installer errors, filesystem paths or credentials.
2. `presenter_ai_hub_install(action=preview,category=tts|asr,id=<catalog-model-id>,token=<unique-nonnegative-int32>)` requests a review. The first status may still be `previewing`; the Core owner performs review asynchronously and emits the same GUI event as before.
3. `presenter_wait(target=ai_hub_install,after_revision=<snapshot.status.revision>,timeout_ms=30000)` waits on the owner's watch channel, not periodic polling. Repeat until a terminal review status (`review`, `wheel_review`, `not_installable`, `failed`); verify `category`, `id` and `token` still match the request.
4. Confirm **only** when `can_install=true` and the result matches the expected selection/token: `presenter_ai_hub_install(action=confirm,category=...,id=...,token=...)`. The underlying installation gate revalidates its snapshot and consumes the approved token exactly once. Returned `operation_id` references the canonical installation journal. Confirm never automatically activates a model or worker.
5. `presenter_wait` can then observe the revision transition to `installing`, `installed`, `cancelled` or `failed`. `wheel` action means only a single reviewed wheel-stage was installed; this is not speech inference readiness.

`--allow-write` is required for preview and confirm because preview changes the Core-owned approval gate. `--core-debug` is **not** needed; these are normal presenter operations. The underlying loopback LocalControl listener is not per-user authenticated. In particular, MCP flags do not confer server-side access control against other local clients.

## Core owner semantics

`AiHubInstallCoordinator` continues to send its original single-consumer `InstallEvent` channel to the GUI. A new watch snapshot is read-only to MCP and is updated by either GUI or MCP operations. The selected plan remains in the existing coordinator's `InstallApprovalGate` under its existing async lock; status is **observational**, never itself authorization to install. A second preview/invalidation revokes the previous gate. The coordinator now reaps already-completed install tasks when a new preview/confirm is attempted, even if no GUI event loop is attached to call `join_finished`. Closed instances reject new previews and confirmations. Errors are mapped to stable sanitized codes.

## Verification and parity

- GUI commands `PreviewAiHubInstall` and `ConfirmAiHubInstall` are both represented through the shared Core installation owner, in addition to six native AI Hub operations from Cycle 10.
- Rust 1.100.0 offline `cargo check --locked --features mcp --lib --bin neuromita-agent-mcp --test local_control`, format checks and diff checks passed.
- Unit tests check rejected installation plans and closed admission; `tests/local_control.rs` checks the **real Core fixture** handling an absent model, status revisions, waiting, and denied confirmation.
- No real external package installed and no full Unity, Slint, ASR or TTS E2E asserted. The main agent must drive actual acceptance.

## Overall GUI Command progress

Based on the 68 source-enumerated GUI commands: **34 fully covered, 11 partially covered, 23 missing (34/68 = 50%)**. This is a GUI `Command` parity measure, not an exhaustive count of all Presenter APIs. Next priorities: speech model/session operations, Telegram/RemoteVoice, Memory Graph and remaining conversation/launcher gaps.
