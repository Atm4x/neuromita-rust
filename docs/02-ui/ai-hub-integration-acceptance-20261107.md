# AI Hub UI integration (agent 5/5) — 2026-11-07

Status: **DRAFT / PARTIAL**, not accepted as full issue #37 completion.

## Git ancestry / provenance

- Upstream proposed base: `Atm4x/neuromita-rust:codex/updater-verified-python-unity-zip-stage-20261107` at `da4edb23dc644218f8ce4fb58e4f692ce68b0f03`.
- Agent 1 (#251): `efc2e610c58eb9735979fc9bea8ed88aefd38b20`, direct child of base.
- Agent 2 (#257): `ace1c885ed2fbc2272418de98e99c798e1b436b6`, direct child of agent 1, used as this integration commit's parent.
- Agent 3 (#258): `d13303802ac8ec2d894a0fb6a9f8682bf1ba7f46`, 4 ahead / 0 behind agent 1; copied byte-identical `settings-panel.slint` and `settings-panel-qa.md` before an explicitly documented adapter gate modification.
- Agent 4 (#259): `24aa7653e30969c960946d70989f952f2676c252`, 8 ahead / 0 behind agent 1; four new Slint paths copied by their original Git blob SHA.
- No unrelated agent branch was merged. `ui/app.slint`, shared shell, and `ai_engine.rs` are the integration-owned changes. Existing settings/other GUI screens remain unchanged.

## Production bindings and safe gating

The catalog's existing `AiHubCatalogState`, component/readiness, settings, and operations v1 models are preserved, not synthesized. The Rust GUI currently opens the hub but **does not supply a verified Core AI Hub catalog/settings/operation adapter**. Both new adapter-connected properties default to false; the settings editor never emits load/save while disconnected, and card/operation mutation buttons are disabled. This is not evidence of installed/ready models, an actual installer, worker, or successful persistence. The GUI mode does not inject fixtures.

- Import agent 2 cards into the active Install tab and preserve all five independent readiness gates, filters and ordering.
- Import agent 3 dynamic schema editor into Settings, route category/tab/Close through its discard/cancel guard, and increment session epoch on each open to fence stale loads.
- Import agent 4 operation panel and queue popup. The queue distinguishes queued/running/history and error/log states when presenter snapshots become available; dialog preflight remains authority-owned.
- The existing GUI `ai-hub-operations` v1 read model remains separately shown by cards; the richer queue uses `ai-hub-operation-snapshots` without inferring unavailable state or translating it optimistically.
- No new backend, Python change, service locator, EventBus, or Generation/TTS cancellation changes.

## Interaction traceability and acceptance — incomplete unless actually executed

| User action | Displayed state | UI callback / event | Outcome | Status |
|---|---|---|---|---|
| Open AI Hub | shell, catalog disconnected warning | `AiEngineUi.open-hub` | native shell shown, no mock | PARTIAL: not GUI-run |
| Switch category | filtered cards / category label | local category key | preserves active settings via guard | PARTIAL: source integrated |
| Install / Settings tabs | separate card/editor view | local tab key | dirty discard/cancel guard | PARTIAL: source integrated |
| Search / backend filter | filtered cards | local predicate | no registry mutation | PARTIAL: source integrated |
| Metadata then late status | independent unknown / ready gates | presenter snapshot needed | no authoritative adapter | BLOCKED |
| Install / update / reinstall | disabled when disconnected | prepare + correlated preflight | no install side effect | BLOCKED |
| Uninstall confirmation | decision modal | confirm/dismiss + token | no backend implementation connected | BLOCKED |
| Required backend preview | decision warning/plan | preflight callback | no verified Core preflight | BLOCKED |
| Queue view | running/pending/history | queue open/close | offline message, no fake tasks | PARTIAL: source integrated |
| Cancel queued/running | disabled when disconnected | distinct cancel callbacks | no actual cancellation | BLOCKED |
| Settings dynamic schema | schema fields, draft/errors | load/save intents | disabled until adapter | BLOCKED |
| Save failure / reset | retain edited draft; reset saved snapshot | correlated schema result | component source only; not exercised | PARTIAL: static |
| Dirty category/tab/close | discard/cancel default | navigation-approved | local guard routed | PARTIAL: not GUI-run |
| Refresh / late events | checking/error snapshot capability | refresh callback reserved | Core status wiring missing | BLOCKED |
| Clear cache / model directory | disabled when disconnected | explicit maintenance callbacks | not wired | BLOCKED |
| RU/EN, focus/esc, scroll | Slint localization, modal focus & scroll | keyboard / I18n | not visually tested | PARTIAL: static |

## Manual/automated QA obligations (NOT RUN in this environment)

`cargo fmt --all -- --check`, `cargo check --locked --offline --features gui --bin neuromita-gui`, `cargo test --locked --offline`, `cargo test --locked --offline --features gui --bin neuromita-gui`, `cargo clippy --locked --offline --all-targets -- -D warnings`, RU/EN desktop launch at 1180×680 and 1560×918, screenshot, keyboard focus/tab/esc, late refresh after category switch, close with dirty edit, vanished component, duplicate actions, save failure/undo/discard and real cancellation. No native build, visual proof or screenshot may be inferred from this PR.

## Dependencies, next work

Core-owned dynamic metadata/status in #59, installation/preflight in #42 and #61, running worker readiness/status in #24. Required: wire existing Core snapshots to the added AppWindow properties and callbacks (including exact component-id, revision, token, operation-id), reject stale callbacks in presenter, produce authoritative action admissions, and then run complete GUI acceptance. Only after those checks can issue #37 be considered for closure.

## Modal scope refinement

The integration mounts the correlated operation confirmation at the full AI Hub overlay level. The operations panel retains its embedded dialog for standalone fixtures, but the production shell disables that embedded instance to avoid duplicate or clipped confirmations. No operation is enabled without an authoritative connected adapter.

## Agent 5 follow-up: real Core speech inventory projection

`SpeechModelCatalog::snapshot()` was already Core-owned and published through `Update::SpeechInventory` for TTS/ASR. AI Hub now consumes that same snapshot, preserving its discovered IDs and verified asset revisions. `Refresh` goes through the existing `Command::RefreshSpeechInventory`. Microphone enumeration failure no longer prevents the independent inventory refresh; its error is still published to Settings. Runtime stop clears the hub catalog.

The projection explicitly distinguishes discovered, package+asset installed evidence, and unavailable worker/inference readiness. A partial package/asset state remains `unknown`, not `installed`; readiness remains `unknown` even when both exist. Runtime/package receipts are **not** proof of load/inference readiness. GUI action gates remain disabled. The read-only status label avoids showing a false "unsupported" verdict for models whose compatibility is not known.

Added pure projection regression tests (`hub_inventory_tests`) for partially installed models, readiness not inferred from receipts, no write actions, and empty inventories. **Tests authored, not run:** Rust toolchain unavailable in this execution environment. This is material progress on metadata discovery, not proof of full #37 acceptance.

## Live Core worker-status projection

The existing `Update::RuntimeStatus(RuntimeSnapshot)` path now additionally updates AI Hub row gates after Voice/ASR. Only an explicit Core `RuntimePhase::ModelReady` with a worker generation and **matching** `active_model.plugin_id` marks the specific model inference-ready; an unrelated model stays unknown. Worker-running with generation can mark only worker readiness, not model readiness. Stopped/Failed is shown separately from unknown/missing status. Service-wide error messages are shown only when the snapshot identifies the same pending/active plugin, avoiding misattribution. Updates apply to all catalog rows regardless of selected category; no new service/event-bus is introduced.

Core status projection regression tests authored: matching vs unrelated plugin, missing generation, absent status, failed worker and source-keyed error. No GUI/runtime compilation has been executed. The Refresh request no longer enters an indefinite optimistic spinner when the bounded GUI dispatch rejects the message.

## Source-revision visual acceptance and software-render regression

Python/PyQt audited read-only at `VinerX/NeuroMita:main` commit `36177e5a559e86d757a0f7bc78c8ef03943eb681` (2026-10-18). Exact source blob SHAs: `dialog.py` `7d48ebba47dd1e266dd8cd56b7f039dca7c5e32f`, `widgets.py` `3ca1405ae3eb71043ba5769e9b3ed5106a44fcff`, `settings_panel.py` `ca2fa0dd9c37234bbb052a41be20ed8959d868b5`, `schema_renderer.py` `1b95e2b7d4079212f26ab91bdd717a862989a103`, viewmodels `ae7dbbdd593e1aaa03f18925606663b5f6503e27` and `36d8736f2afddb09bdfaaa2a3ff433b250944b60`, styles `d852b441319ce8ab7fb074bbc6ea440354c0ab69`. This snapshot is a read-only source audit, not a live PyQt app run.

The existing software-render GUI test now exercises the real AppWindow AI Hub modal at **1180×680/RU** and **1560×918/EN** using an explicitly test-only SpeechInventory fixture with partially installed TTS and not-installed ASR rows. It asserts catalog rows and a correlated Worker/ModelReady snapshot; saved .ppm captures are uploaded by fork CI as `ai-hub-captures` when the test executes successfully. Manual Windows 10 keyboard/focus/esc and operational install/E2E remain unverified.
