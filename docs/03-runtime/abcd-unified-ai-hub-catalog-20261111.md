# ABCD iteration 7 — Unified implementation-aware AI Hub catalog

Base: `6f039276d483e294adf685670fe639d275d78c50`. Target PR: #416 (`codex/abcd-core-boundary-fixes-20261110`). `main` is out of scope.

## Invariants

1. One application-owned worker process per **service**. Python and Rust are immutable model implementation metadata, not selectable modes or global settings.
2. One display name or `model_id` does not uniquely identify a model. GUI identity is the tuple `(service, model_id, implementation, revision)`; native revision is its exact publisher-approved manifest SHA-256.
3. The catalog is a read-only projection of two existing Core inventories, not another owner of models, worker processes, trust policy, or installation operations.
4. Signed native release approval does **not** prove that weights, runtime assets or a worker are ready. Installed native metadata is a manifest observation only; the live Core-owned worker is the authoritative readiness source.
5. Installed native service category derives from manifest `speech_capabilities`. An approved but not installed revision has no trusted service declaration and is displayed in Else until its manifest exists. No guesswork from `model_id`, publisher or backend.
6. Each UI command resolves a fully qualified service/model/revision; the native service verifies the pinned SHA **before process spawn** and again compares the active session identity **before stop**, under its service transition gate. A stale UI cannot run or stop a different revision.

## Refactoring

- Core native revision inventory includes declared speech services from the installed manifest. The publisher-authorized release presenter joins release approval to the exact installed manifest digest.
- `UnifiedHubCatalog` joins Python inventory with publisher-authorized native revisions. Duplicated names and ids across implementations or revisions remain separate. Category counts cover both implementations. Data is refreshed from either source independently.
- GUI's `HubCatalogState` retains the last two immutable Core inventories; it does not own any runtime process, installation state or model activation. Slint displays one TTS/ASR/Core/RAG/Else model list without a Python/Rust selection tab. Implementation is shown as model metadata.
- Native package download, revision selection and lifecycle controls appear in the selected native model's detail page, filtered to its exact model/digest. Python install review is only available for Python entries.
- Native GUI `activate` and `stop` commands are scoped to the concrete service and revision. They call `AiHubNativeService::activate_exact_for_service` / `deactivate_exact_for_service`, not global activation/shutdown that might impact another service.
- Added tests for identical ids/display names across implementations, multiple native revisions, unknown services for not-yet-installed manifests, and one manifest with multiple declared speech services.

## Verification and limitations

See the accompanying iteration report for actual test counts and Git Bridge publication. GUI compilation in the current 4 GiB Linux sandbox has historically been OOM-killed even with debug symbols disabled. Until a larger-runner or Windows GUI build passes, do not claim full GUI acceptance.

Still open: native Embedding/Reranker operation capability contracts and working implementations; cross-process SQLite/Timeline writer admission during RAG external-provider dispatch; live signed model and Unity tests; strict GUI Clippy and complete localization. Do not merge PR #416 to main until the remaining acceptance gates pass.
