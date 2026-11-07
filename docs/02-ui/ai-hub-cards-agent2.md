# AI Hub cards (agent 2/5) — GUI-only staged slice

Refs #37. **Base:** agent 1 shell PR #251, exact parent `efc2e610c58eb9735979fc9bea8ed88aefd38b20` (`codex/aihub-ui-shell-20261107`). This is an additive GUI view, not an install backend, a worker, a fake model catalog, or a claim of completed #37.

## Implemented

- `ui/ai_hub/cards.slint` displays the supplied `AiHubComponent` with title, description, metadata chips and five **independent** truth gates. An installed component cannot appear inference-ready unless the presenter explicitly supplies `model-ready=yes`.
- Discovered/supported/installed/environment-ready/worker-ready/model-ready and loading/checking/error are independent. Unsupported reason, status error and matching queued/running/cancelling/failed operation detail render as wrapped text.
- UI events use exact `component-id`, verb, `revision` and `token` from the source model. Clicks do not set installed, ready, or success. The local request latch prevents repeated clicks on the same mounted card until a newer same-token owner snapshot; another component's card is unaffected.
- Buttons: Install, Update, Reinstall, Uninstall and Settings. The v1 contract permits only `can-install`, `can-uninstall`, `can-open-settings`. Therefore Update/Reinstall have separate *opt-in, default-false* card properties, **not** guessed from installed or can-install; the shell leaves them off until a presenter extends admission. Unsupported install/uninstall are disabled. An external `operation-busy` gate is exposed to the presenter.
- Shell uses cards in Install tab with supported/model-ready-first visual grouping; the original `components` model is not sorted or mutated. Original category/ID/revision are preserved, and rows with status still unknown are retained.
- All text gates and actions respond to I18n locale change without reconstructing the component list. No production fixture or side effect.

## Required follow-ups / not claimed

The shell v1 accepts only `[AiHubComponent]` and `[AiHubOperation]`. Slint 1.18.1 cannot transform this model with normal `map/filter` expressions, and the contract carries neither language/RAG-group metadata nor explicit update/reinstall permissions. Full query over **all tags/languages/status text**, accurate per-category/result counts, RAG embeddings/reranker grouping, per-component busy derived from operation rows, and per-action update/reinstall admission need a typed **presenter projection** and a reviewed v1 contract extension. Do **not** fill these gaps with fabricated model data or enable destructive actions heuristically. The legacy shell search remains a metadata-first partial filter; CPU/none filter exceptions remain intact.

The component itself does not apply asynchronous status packets: the presenter must reject mismatched `component-id + revision + token` before replacing a row. The click latch is only a UX guard, not admission, a queue implementation or durable idempotency.

## Verification checklist (not executed here)

1. 0/N rows and catalog unavailable: no fabricated cards or green readiness.
2. Metadata-only row, unsupported row, installed-but-not-ready, checking, failed: correct independent gates and text; no unauthorized actions.
3. RU ↔ EN while loaded: same source IDs/revisions and no rewritten production catalog.
4. Search/category/backend changes: preserve read model; CPU/none remain visible under CUDA/ONNX; verify missing tag/language search separately.
5. Late status for B while A selected: only B revised by presenter; stale token/revision cannot overwrite newer row.
6. Double-click Install: exactly one request until owner advances; no local success. Two different components may produce separate requests. A matching running task must keep its component gated by presenter.
7. Long status/reason/detail and many chips at 1180 × 680: text readable; test narrow window and keyboard focus.

`cargo fmt --all -- --check`, `cargo check --locked --offline --features gui --bin neuromita-gui`, `cargo test --locked --offline`, `cargo clippy --locked --offline --all-targets -- -D warnings`, Slint GUI screenshot and manual QA **not run**: no Rust toolchain, complete working tree or display renderer in this execution environment. This PR must remain draft until native verification and presenter integration.
