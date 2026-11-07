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
