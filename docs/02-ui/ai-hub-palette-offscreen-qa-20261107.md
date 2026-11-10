# AI Hub palette and offscreen visual QA — 2026-11-07

## Scope and source audit

This GUI-only follow-up is stacked on the PyQt-audited AI Hub shell. The source references remain:
`Atm4x/NeuroMita/src/ui/windows/ai_hub/{dialog.py,widgets.py,settings_panel.py,schema_renderer.py,constants.py}` and `src/styles/ai_hub_styles.py` on Python `main`. The existing source-to-Slint mapping is in `ai-hub-pyqt-to-slint-audit-20261107.md`.

The user-approved adaptations are retained: first-class sidebar page, top-level TTS/ASR/Core/RAG/Else category tabs, and a per-card settings gear opening a nested page in the same catalog surface. No absolute x/y positioning, negative offsets or hand-positioned icons were added.

## Shared semantic palette

The initial C4 shell used `Theme` for base colors but repeated **16 ad hoc hexadecimal literals** across `ui/pages/ai-hub.slint` and `ui/ai-hub/components.slint`. This revision removes all inline hexadecimal colors from those two files and places shared tokens exclusively in `ui/theme.slint`, including:

- `surface-quiet`, `surface-disabled`, `surface-soft`, `surface-hover`
- `accent-surface`, `accent-surface-hover`, `accent-label`
- `warning-outline`, `warning-surface`, `success`, `success-outline`, `danger`, `unsaved-confirmation`

Existing `Theme.background`, `.panel`, `.border`, `.muted`, `.text`, `.accent`, `.accent-hover`, `.accent-border` continue to serve as the primary palette. Model cards, toolbar, pills, category selection and details now reference shared theme tokens; no forked AI Hub palette has been introduced.

## Data ownership

Removed the hardcoded demonstration model catalog from the production `AiHubPage`. The page now exposes `in property <[HubModel]> entries`; with no presenter connected it renders a truthful empty state. The Rust frontend does not invent a list of installed models or readiness states. A separate temporary standalone preview outside the repository injects eleven **non-production illustration rows**, never persisted or included in the PR. The `Else` display category groups `voices`, `extras`, `beats`, `deps` and `dependency`; `Core` also recognizes the original PyQt `backend` source category.

## Real offscreen visual testing

A small standalone Slint preview compiles only `ui/pages/ai-hub.slint`, its reusable components and the shared theme. It is built with Rust 1.98.1 and the vendored Slint 1.18.1, without compiling the full application, presenter or IPC.

Use `Xvfb :91 -screen 0 1440x900x24 -nolisten tcp` and run the Slint preview with `DISPLAY=:91 SLINT_BACKEND=winit-software`. Capture actual pixels from that X server via `DISPLAY=:91 scrot -z filename.png`. The model gear is tested using X11 XTest mouse events to open the nested details page. The screenshots are **actual software-rendered Slint UI**, not generated mockups. They are available in the associated ChatGPT session as `hub-list-final.png` and `hub-settings-final.png` and intentionally are not checked into the source tree.

Visual QA detected and fixed two defects: backend/language chips stretching to fill their whole row instead of fitting the content, and search lacking visible placeholder text. `HubPill.width` now derives from `pill-label.preferred-width` and the search field shows a localized Russian placeholder in a declarative layout. Both fixes were verified by reshooting after rebuilding the isolated preview.

## Verification, exclusions and follow-up

- Isolated Slint GUI preview build: PASS; no `x/y` layout positioning.
- Main application Slint typecheck: run `cargo check --locked --offline --features gui --bin neuromita-gui`; result attached to handoff.
- Only visual UI, theme, and typed `HubModel` input were modified. No backend service calls, no persistence, no settings presenter, no new production sample catalog, no runtime installation/Remove buttons.
- The visual snapshots demonstrate one full-size desktop viewport. Responsive/narrow-mode screenshots, actual live-schema forms, integrated main-app sidebar, and Rust presenter binding remain separate future acceptance targets.

### Category-switch layout regression

An additional X11 interaction test switched from TTS to ASR after returning from per-model settings. It exposed that Slint `visible: false` on a repeated `HubModelCard` did **not** collapse its `min-height: 124px`; inactive categories reserved vertical space and pushed ASR cards near the footer. The corrected card delegate binds its `min-height`, `preferred-height` and `max-height` to zero for nonmatching items, and to the intended 124px for visible cards. A fresh rebuild and software-rendered ASR screenshot (`hub-asr-qa-collapsed.png`) confirmed both Whisper cards now begin directly under the toolbar. No absolute positions or negative margins were introduced.
