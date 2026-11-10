# C4: first Core-backed overlay installation from AI Hub

## Implemented boundary

The AI Hub has an explicit two-step consent flow after an existing real Python SDK install-plan review: **Review → Install overlay → Confirm**. The GUI sends only the `(service, plugin ID, current token)` to Core. It cannot choose or supply packages, backend, receipts, or asset paths.

Core owns a one-use `install_admission::InstallApprovalGate`. A new review, catalog refresh, changed selected model, or attempted consume clears the previous approval. An install operation receives a correlation UUID and an actual result from `PluginInstallHandler::install_with_approval` and the existing immutable `OverlayInstaller` journal and staging owner. No parallel operation is admitted by the GUI runtime. The result describes only the plugin overlay, **not** loaded inference readiness.

`install_admission::install_reviewed_overlay()` reruns the real installed Python SDK plan, checks the pinned plugin source, SDK revision, managed CPython 3.12, settings and full plan fingerprint, and uses that newly evaluated plan for installation. It rejects source or plan drift. It does not invoke `AiProductionInstaller::prepare()` or start any worker. A deliberately narrow safety gate allows only a builtin `remote-api` plan **without** packages/requirements, model assets, supplemental backends, install actions or generation hooks. Everything else remains review-only with a clear explanation rather than guessing a backend or model revision. UserData and cache paths are derived from the injected portable root.

## Known limits and required next milestones

- This is real publication of a **dependency-free remote API plugin overlay**, not a general TTS/ASR model installer. ONNX/CUDA, model weights, package resolution, dynamic settings, generated assets and destructive Remove are not supported by this UI operation.
- The operation UUID/status currently live in the GUI process. There is no durable cross-restart operation log, multi-stage progress, or guaranteed cancellation after commit starts. `OverlayInstaller` has staging/lease cleanup; app-shutdown and native Windows recovery need dedicated tests before those properties can be claimed.
- Python `installplan.py` is executable code; only pre-verified builtin sources are allowed. A subprocess is not an OS sandbox, and source fingerprints cannot replace a permission/security boundary for third-party Python.
- `PluginInstallHandler` owns actual overlay publication and dependency semantics. C4 does not add another installer, storage schema, event bus, persistent catalog, or supervisor. Caller approval is one-shot; replay or changed plan is rejected.
- Install dialog layout must be verified in actual Slint renderer at both 1560×918 and 1180×680. Full production runtime E2E and Windows/macOS remain open.

## Acceptance checks

`cargo test --locked --offline --lib ai_runtime::install_review::tests -- --test-threads=1` exercises remote-only plan admission, one-use approvals, real embedded Python SDK install-plan evaluation on a temporary builtin fixture, immutable overlay publication, idempotent retry and denial after source/plan drift. `cargo check --locked --offline --features gui --bin neuromita-gui` is a production typecheck, not an executable GUI E2E.

Refs #37. Keep this stage stacked on prior C4 PR #297; no `Closes #37`.
