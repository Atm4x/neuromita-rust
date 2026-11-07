# C4 AI Hub: read-only Core catalog presenter

Parent: PR #279 (`ad9334cd0655ceccefd873b3165f9dc5b62e1c89`). Refs #37.

## Data ownership and event path

`DesktopRuntime` owns the active `SpeechModelCatalog` and produces `SpeechInventory` on startup and on `RefreshSpeechInventory`. `sandbox::apply` gives the **same** inventory to `voice`, `asr`, and `ai_hub` presenters. The AI Hub presenter projects it into `HubModel[]` and five category counters in one update on the GUI event loop; neither Slint nor the presenter persists another catalog. Replacements replace the full list and its counters rather than accumulating stale rows. TTS and ASR can have equal plugin IDs without index aliasing because their categories remain separate.

The `AiHubPage` receives `ai-hub-entries`, `ai-hub-category-counts`, and `ai-hub-can-refresh` through the existing `AppWindow` composition. The refresh callback dispatches the existing `RefreshSpeechInventory` command only while an inventory source is available; it becomes unavailable on runtime stop. An audio-device enumeration failure reports the error but does **not** suppress independent speech catalog discovery.

## Truthful state model

`SpeechModelOption` supplies only `package_detected` and `verified_assets`, so the presenter distinguishes four states: `discovered`, `package-only`, `assets-only`, and `package-assets`. **None is worker-ready/model-loaded/inference-ready.** Real catalog entries must never receive the UI's demo-only `ready` status from these flags. Model backend, language, size, install location and settings schema are unknown at this boundary, and are not fabricated. The detail view is read-only with an explicit unconnected-schema message; Install/Remove/Save remain disabled.

Category counts are derived from the same inventory as the cards. Core/RAG/Else remain zero until a corresponding Core-owned inventory type and projection exist. Unknown backends do not match NVIDIA/ONNX/CPU selectors. Unrecognized/invalid plugin manifests are filtered by the Core catalog, not reclassified as valid in the UI.

## Verification boundary

`cargo check --locked --offline --features gui --bin neuromita-gui` and `cargo check --locked --offline --features gui --bin neuromita-gui --tests` validate generated Slint bindings and Rust types, **not** operation readiness or a functioning production installer. The `ai_hub::tests` exercise category counters, duplicate IDs across categories, package/assets classification, and replacement snapshots. Renderer snapshots verify tab count badge alignment against title plus subtitle; the preview uses demo entries and is not the production catalog.

Remaining: Core-backed dynamic category metadata beyond speech, settings schemas/edit validation/save/revert, verified runtime status per model, install/remove operation queue, runtime GUI E2E, late refresh fences and Windows/macOS behavior. The independent draft #262 is not merged into C4.
