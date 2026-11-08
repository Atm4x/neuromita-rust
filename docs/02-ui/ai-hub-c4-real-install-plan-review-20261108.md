# AI Hub C4 — validated install-plan review (Agent C)

This stage is a **real Core-backed installation-plan request**, not installation itself. The AI Hub model details page has a `Проверить план` action. The user can inspect the backend choices, locked packages/requirements count, required/optional model assets, explicitly approved installer actions and asset-hook count, plus a shortened fingerprint. No progress, readiness, package installation, or approval is fabricated.

## Ownership and call path

`AiHubPage.preview-install-requested(HubModelIdentity, token)` → `AppWindow.ai-hub-preview-install` → `ai_hub::bind` (unique current `(category,id)` check) → `runtime::Command::PreviewAiHubInstall` → latest `SpeechModelCatalog::snapshot` selection check → `ai_runtime::install_review::preview_installed_environment` (in its own async task) → `Update::AiHubInstallPreview` → Slint.

The Core preview verifies the executable plugin source, portable plugin root, embedded-SDK active publication, exactly one installed managed Python 3.12.x version and its actual `--version`, then obtains the plugin-provided plan through existing short-lived Python SDK host IPC. It calls `PluginInstallPlan::validate`, rejects missing backends, and creates a SHA-256 fingerprint bound to service, plugin ID, inspected manifest/install contract bytes, SDK revision, selected Python version, settings and plan. Only the default `{}` settings are currently supported in UI; a dirty placeholder settings draft disables preview. Real dynamic settings editing is a separate gate.

The asynchronous `InstallReviewFence` invalidates previous requests on a new request or catalog refresh, aborts the previous task and ignores late results. Slint additionally checks request token and selected `(category,id)` and resets review on model change, dirty draft input, settings closure, and inventory refresh. A refresh cannot silently approve a previous plan.

## Security and explicit limitations

**This preview does not call `AiProductionInstaller::prepare`, bootstrap Python, resolve/download packages, publish overlays, install backends, generate model assets, or initialize inference.** It uses an already installed verified SDK and interpreter and fails closed when they are unavailable. No system-profile location is used for persistent AI data.

**The Python `installplan.py` script really executes during preview.** A short-lived subprocess is *not* an OS sandbox; arbitrary Python code could still have filesystem/network side effects. Therefore this stage deliberately restricts the review action to a manifest-validated executable **builtin** plugin in the portable root, excludes `community` plugins, and does **not** claim a general side-effect-free sandbox. Trusting installed plugin source bytes remains a required future attestation gate; the computed fingerprint is a display/review freshness identifier, **not a signed authorization**. No raw Python exception, settings JSON, environment paths, or secrets are sent to the UI/logs.

No `Install`, `Remove`, `Save`, cancel or backend-select operation is enabled here. The UI review intentionally has no approval button because there is not yet a Core-owned immutable request/operation ID and safe acknowledgement. A future installation must reverify the current source, SDK and plan under its own owner and transaction/lock before making any filesystem changes. The current review fingerprint alone must **never** be accepted as permission to install.

## Verification and follow-up

Scoped executed library tests cover the real SDK host and test Python install contract, refusing to write overlay/backend/model outputs, missing portable SDK, unique managed version, plan/backend/asset validation, fingerprint changes and request-generation fences. Production GUI and GUI-tests typechecks establish binding compatibility, but do not establish runnable production installer E2E. Render QA remains separate from the actual Rust command channel.

Next gates: (1) package/builtin source provenance and managed runtime bootstrap UX, (2) typed dynamic model settings validation, (3) immutable approved install plan with explicit backend/asset/source/version choices, (4) one Core-owned operation with correlated progress/result/cancel and rollback, (5) native Windows and application lifecycle acceptance. Destructive Remove remains blocked by separate C3 lifecycle proofs. Independent UI draft #262 remains unmerged.
