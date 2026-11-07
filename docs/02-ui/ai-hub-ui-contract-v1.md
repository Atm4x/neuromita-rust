# AI Hub — typed Slint UI contract v1 (agent 1/5)

**Scope:** GUI shell and boundary only; `Refs #37`, not a claim of completed AI Hub. Reference baseline: upstream `Atm4x/neuromita-rust` PR #244 `codex/updater-verified-python-unity-zip-stage-20261107`, commit `da4edb23dc644218f8ce4fb58e4f692ce68b0f03`. Slint pinned `=1.18.1` in Cargo.toml. The base was chosen because it descends from the current GUI/onboarding branch, preserves SettingsPage/Sidebar and has previously reported locked GUI check; it is **not** selected merely because its PR number is large. A real build of this patch is still required.

The Python references are `VinerX/NeuroMita/src/ui/windows/ai_hub/{dialog,constants,presentation}.py` and `src/styles/ai_hub_styles.py`; behavioural audits: `docs/02-ui/ai-hub.md` and `docs/02-ui/ai-hub-interaction-traceability-checklist.md`. This contract does **not** port their installer, queue or Python Core.

## Types and ownership

All types are exported from `ui/ai_hub/contract.slint`, then re-exported from `ui/app.slint`. Field names below use Slint's kebab-case syntax (generated Rust fields use underscores).

| Exported type | Fields / semantics | Owner / consumer |
|---|---|---|
| `AiHubTruth` | `unknown`, `no`, `yes`. Used independently for each readiness gate, never a single `ready` bool | Presenter supplies; cards (agent 2) render |
| `AiHubLoadPhase` | `unavailable`, `idle`, `loading`, `checking`, `loaded`, `failed`; error separate | Presenter supplies, shell/cards/settings render |
| `AiHubCatalogState` | `available` means **adapter available**, not installed/model ready; `phase`, `error`, `revision`, `token` | Rust GUI presenter supplies; shell owns no source of truth |
| `AiHubComponent` | stable `component-id`, raw `category`, `title`, `description`, `tags`, `backend`, `compatibility`, `supported`, `unsupported-reason`; independent `discovered`, `installed`, `environment-ready`, `worker-ready`, `model-ready`; `phase`, `error`, `revision`, `token`; `can-install`, `can-uninstall`, `can-open-settings` | Core/presenter supplies metadata/status and capability decisions; agent 2 owns interaction; no hardcoded model catalog |
| `AiHubSettingKind`, `AiHubSettingOption`, `AiHubSettingField`, `AiHubSettingValue` | typed schema: kind/required/enabled/choices; value and field error keyed by schema key. `value` wire format is string, **not** unvalidated direct storage access | Presenter translates from Core schema and validates; agent 3 owns schema renderer |
| `AiHubSavePhase`, `AiHubSettingsState` | `component-id`, `schema`, `values`, `phase`, `dirty`, `save-phase`, `error`, `revision`, `token`. Draft vs saved remains **presenter-owned**; save failure must preserve dirty draft | Agent 3 (editor) and Rust presenter |
| `AiHubOperationPhase`, `AiHubOperation` | stable `operation-id`, `component-id`, `action`, `queued/running/cancelling/succeeded/failed`, optional progress (`progress-known`, 0–100), `detail`, `error`, `revision`, `token`, `can-cancel` | Agent 4 (queue/status) consumes; Core owns admission and lifecycle |

The shell has props `catalog`, `components`, `settings`, `operations`, `selected-category`, `active-tab`, `search-text`, `backend-filter`. It reuses `SettingsTheme`, `I18n` (with explicit English fallback for new labels), `ActionSurface` and `SettingsScrollView`. Static **grouping keys** only: `tts`, `voices`, `asr`, `rag`, `extras` (including `beats`), `backend`, `deps` (including `dependency`), plus `other` for unrecognized categories and `all`; original category is retained. The visual filter currently searches metadata text; **agent 2** must cover tags, languages and status in its definitive card search implementation. CPU/none are permitted under accelerated backend filters, matching audited behaviour. Unknown components must never be silently dropped.

## Callbacks / async rules

| Callback | Arguments | Gate / producer / owner |
|---|---|---|
| `close-requested()` | none | Shell to AppWindow; currently refuses while `settings.dirty` or saving. Agent 3 must implement Discard/Cancel confirmation for all close paths, including native hide |
| `refresh-requested(revision, token)` | snapshot revision/token | Reserved; agent 2/presenter binds only after adapter exists |
| `component-action-requested(component-id, action, revision, token)` | stable ID, explicit verb, source snapshot | Reserved; agent 2 binds with capability-checked `can-*`, never infers support from visual state |
| `settings-save-requested(component-id, revision, token)` | selected stable ID + form snapshot | Reserved; agent 3 binds after schema validation; dirty draft cannot disappear on failure |
| `operation-cancel-requested(operation-id, component-id, revision, token)` | task + owning component + snapshot | Reserved; agent 4 binds after real task admission |

`revision` is a monotonic owner-issued sequence and `token` an opaque request/selection identity. **Both** must be checked by the presenter before a late result is applied; do not compare only category or array index. A result for a deselected component, old `token`, or older revision may be observed/logged but cannot overwrite the current editor or advertise readiness. New revisions may update unrelated rows while preserving a dirty form for the selected stable component ID. A completed install/worker handshake does not imply `model-ready`. `can-install/can-uninstall/can-open-settings` default false; unsupported actions always disabled. Refresh and write actions remain unbound in shell v1.

`AiHubCatalogState.available` defaults to false in production. `components` defaults to empty, and no Slint-owned fixtures are installed on startup. Absence of an adapter is rendered explicitly as **data/control unavailable**. Example/mock content may only be injected through a separate, explicitly invoked CLI screenshot-preview, never persisted; this patch deliberately does not add such a mode.

## File ownership / parallel edits

| Owner | Allowed files | Must not change |
|---|---|---|
| Agent 1 (this patch) | `ui/app.slint` minimal overlay binding, `src/bin/neuromita-gui/ai_engine.rs` existing AI Engine entry callback, `ui/ai_hub/shell.slint`, `ui/ai_hub/contract.slint`, this doc | Sidebar/Nav schema, Core, workers, installer, arbitrary settings state |
| Agent 2 | `ui/ai_hub/cards.slint` (new), card presenter adapter/wiring after coordinating contract | Agent 1 shell layout/contract without interface review |
| Agent 3 | `ui/ai_hub/settings*.slint` (new), schema adapter, draft and close guard | Another agent's cards/queue |
| Agent 4 | `ui/ai_hub/operations*.slint` (new), operation presenter adapter/status/queue | Installer/Core/runtime code absent explicit coordination |

The three future agents should open a small interface change PR against `contract.slint` when v1 is insufficient, not expand `AppWindow` with duplicated mutable stores. Read models and callbacks define the **UI boundary**, not application business logic.

## Acceptance matrix and honest gaps

| Check | State for this portable patch |
|---|---|
| Open from AI Engine settings, switch categories/tabs, close | Declared in Slint, requires runtime smoke test |
| UI dimensions ~1180 × 680, scroll/focus, RU/EN | Responsive layout and existing localization hooks used; **not visually checked** |
| No false installed/worker/model readiness at startup | Empty catalog + `available=false`; no production fixture |
| `cargo fmt --all -- --check` | **Not run**, Rust toolchain absent in packaging environment |
| `cargo check --locked --offline --features gui --bin neuromita-gui` | **Not run**, Rust toolchain and vendored clone absent |
| `cargo test --locked --offline` | **Not run** |
| `cargo clippy --locked --offline --all-targets -- -D warnings` | **Not run** |
| PR / CI on exact patch HEAD | **Not created**: connected GitHub account has read-only permissions on fork and upstream, and local network cannot resolve github.com |

Before merge: apply patch in an exact checkout of the baseline SHA, run `git diff --check`, the project-required offline cargo checks, keyboard/mouse/wheel/resize and RU/EN, then publish a proper PR with `Refs #37`. **Never claim these absent checks passed.**

## Handoff / exact refs

- `base_repo`: `Atm4x/neuromita-rust`
- `base_branch`: `codex/updater-verified-python-unity-zip-stage-20261107` (PR #244).
- `base_sha`: `da4edb23dc644218f8ce4fb58e4f692ce68b0f03`.
- Intended writable fork from task: `VinerX/neuromita-rust`; connected GitHub principal has `pull=true`, `push=false` there, so no remote `head_sha` or PR exists.
- Patch files only: `ui/app.slint`, `src/bin/neuromita-gui/ai_engine.rs`, `ui/ai_hub/contract.slint`, `ui/ai_hub/shell.slint`, `docs/02-ui/ai-hub-ui-contract-v1.md`. Changes outside this scope: **none**.
- GitHub CI results for this patch: **none**. Previous base-PR CI and checks are not proof for this patch.
