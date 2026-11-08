# AI Hub Settings panel (agent 3/5) — interface and QA ledger

## Scope and provenance

Source: issue `Atm4x/neuromita-rust#37`, `docs/02-ui/ai-hub.md` Pass 13, PyQt `VinerX/NeuroMita/src/ui/windows/ai_hub/{schema_renderer,settings_panel,settings_presentation,status_widgets}.py` and `src/controllers/gui/ai_hub_settings_view_model.py`.

Implementation: **GUI-only** `ui/ai_hub/settings-panel.slint`, stacked on the exact shell commit `efc2e610c58eb9735979fc9bea8ed88aefd38b20` (PR #251). Does not import itself into the shell because `ui/app.slint`, `ui/ai_hub/shell.slint`, and public `contract.slint` are other-agent ownership. It is **not yet exposed in the running application**. There is no Python runtime adapter, storage write, compilation, or model initialization.

## Adapter/host integration contract (must be reviewed with agent 1)

* Feed `components` with the real, current category registry. The panel displays rows whose `installed=yes` **or** `can-open-settings=true`; there is no hardcoded component catalog. The host must provide category-filtered rows or use the `category` property; it must not infer readiness from installed.
* When `load-requested(component_id, component_revision, token)` fires, load dynamic schema and stored values via Core/presenter. Echo the **exact token** in `incoming.token`, use monotonically increasing `incoming.revision`, and validate the selected ID. The host must set a fresh, distinct nonempty `session-token` on every show/reopen. Late responses from an old selection, older revision or prior session are rejected.
* Normalize incoming `schema` and `values` to matching **index/key order**. Append unrecognized persisted keys as read-only rows rather than dropping them from the save payload. This is required because Slint can update a repeated model element by index but has no imperative model iteration in functions. Keep backend error keys attached to the same value row. Preserve the entire `[AiHubSettingValue]` array on save; never take schema defaults as saved values.
* `hints` are local GUI-only, index/key-aligned supplemental projection of *real* schema: `raw-type`, `secret`, `unknown-type`, numeric `bounded/minimum/maximum/step/suffix`, and simple `has-dependency/dependency-index/dependency-value`. Presenter should evaluate richer Python behavior rules and send final `field.enabled`/forced values; UI must not reimplement domain validation. `unknown-type` must be true when mapping an unsupported schema type into v1 `AiHubSettingKind.text`.
* On Save the panel emits `save-requested(component_id, draft_values, revision, token)`. The backend is authoritative for validation and persistence. Return `incoming.save-phase=saved` with canonical saved values, or `failed` with per-key `value.error` and `incoming.error`. The panel keeps the draft on failed saves; it adopts the returned canonical saved snapshot on success.
* Host navigation MUST call `request-category(id)`, `request-tab(id)`, and `request-close()` before updating shell state or hiding its window. Only `navigation-approved(action, target)` permits actual external navigation. This **includes native hide-on-close, Escape/window-manager close, and modal dismiss**. Shell v1 does not yet do this; current shell's Close button is simply disabled on dirty rather than showing this guard. Wiring requires owner review.
* If a component disappears during an edit, the host sets `active-component-present=false`; the panel retains draft and disables Save. Discard/Cancel still work. The component list may refresh in-place without resetting a dirty editor.
* Fish Speech+ UI appears for the two **documented** component IDs `tts:medium+` and `tts:medium+low` only; all device/target/cache/metadata strings come from `compilation`. Buttons stay disabled if `runtime-data-present=false`. `compile-requested` passes the draft so the backend can save device settings first; `clear-compilation-requested` is separate and does not demand save. Both require confirmation. Neither performs work itself.

## PyQt renderer mapping and differences

| PyQt schema renderer | Slint GUI projection | Gap/limit |
|---|---|---|
| `entry` / legacy default | `text` → `LineEdit` | password only when actual schema is marked secret by the presenter |
| `combobox`, `values` + `display_labels` | `choice` → expandable list of dynamic value/label pairs | current selected value displayed raw; labels for selected row not resolved without adapter-supplied presentation |
| `checkbutton` (alias `check`) | `boolean` → `CheckBox`, `True`/`False` wire values | Python accepts broader truthy string spellings; Rust adapter should canonicalize on load |
| `spinbox`, `number_stepper` | `integer` / `decimal` → text input + step buttons; min/max visual check and bounded nudge | does not replicate native QSpinBox rounding/step alignment or every locale/precision case; backend validates |
| unknown `type` | v1 `text` + `hints.unknown-type` → read-only raw value | adapter must preserve unrecognized value and avoid substituting defaults |
| `locked`, `enabled=false`, conditional `behavior` | `field.enabled`; optional simple equality dependency | Python `evaluate_setting_behaviors` can force values; complex behavior evaluation stays with presenter/Core |
| `help/help_ru/help_en` | `field.description` inline | separate native tooltip not yet implemented |
| `presentation_slot=fish_speech_compilation` | independent Fish Speech+ compilation card | does not run compilation/clear or validate devices |

Important parity caveat: PyQt `load_settings()` can implicitly migrate and save voice-device values; this GUI intentionally **does not** do any such writes. Also the Python catalog can swallow loader exceptions and yield empty schema; the adapter must distinguish known no-schema vs actual error when possible.

## Test scenarios to execute after shell wiring and compiler availability

All states below are **test specifications, not PASS reports**:

1. Load A token `session1:1`, switch to B token `session1:2`; deliver late A result then fresh B result. Verify only B is visible. Reopen under `session2`; old result must not change form.
2. Send schema/values for B with required or bounded invalid input and backend errors on `key1` plus global `_`. Verify both error surfaces appear, without discarding the invalid draft.
3. Change B draft, refresh same-category rows, then receive newer loaded snapshot with the same ID and token. Verify draft remains; revision update does not overwrite it.
4. Trigger Save, send `failed`, verify draft and dirty remain. Send success on retry, verify saved snapshot and dirty cleared.
5. Load value `custom` with schema factory default `factory`, edit to `other`, Reset. Must restore `custom`.
6. Change component, tab, category and request close while dirty: test **Cancel** retains draft and location; **Discard** restores saved and approves transition. Test native hide-on-close through host.
7. Attempt component/tab/close switch while Save is pending: all navigation must be blocked.
8. Give unknown schema type and persisted value: row must be read-only; a Save of other fields must still include unknown key/value.
9. Fish model with runtime absent: compile/delete disabled and explanation visible. With explicit target data: confirm compile and clear separately; verify only callbacks, never runtime calls.
10. GUI run at about 1180×680 in RU and EN; test scroll wheel, keyboard tabs/Enter/Escape, focus on confirmation Cancel, long labels, large option list and secret masking.

## Current validation and blockers

* Git changes published via GitHub API; exact commits and diff/CI verified in PR metadata (see PR).
* **NOT RUN**: `cargo check --locked --offline --features gui --bin neuromita-gui`, `cargo fmt --all -- --check`, `cargo test --locked --offline`, `cargo clippy --locked --offline --all-targets -- -D warnings`, and native GUI screenshots. This environment has Git but cannot resolve github.com from git and does not have Rust/cargo installed.
* An unreferenced Slint file is not compiled by the existing GUI build. Real typed binding, compile checks, QA, keyboard focus and screen-reader acceptance are therefore **blockers**. Do not report this standalone source as a production-integrated editor or issue #37 closed.
