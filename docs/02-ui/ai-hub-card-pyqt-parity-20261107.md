# AI Hub ModelCard: PyQt → Slint layout parity

Source of truth: `Atm4x/NeuroMita` on `main`, `src/ui/windows/ai_hub/widgets.py`, `ModelCard._build` lines 255–440, and `src/styles/ai_hub_styles.py` lines 285–315, 397–452. This document addresses presentation only; the presenter and installation workflow are deliberately unchanged.

| PyQt ModelCard construction | Slint implementation |
|---|---|
| QHBoxLayout margins `(18, 14, 18, 14)`, spacing `14` | Root `HorizontalLayout` with the same margins and spacing |
| `info_col`: title (14px/700), description (11px), 4px gap, `addStretch(1)`, `root.addWidget(..., 3)` | Left `VerticalLayout`, top-aligned, 3 units horizontal stretch; no midline centering of text |
| `chips_row`: 6px spacing, `AlignVCenter | AlignRight`, `root.addWidget(..., 4)` | 4-unit stretching metadata container with vertically centered, right-aligned, one-line pills |
| Not installed: fixed `18 × 18` status icon; installed: compact green status label | Fixed-width OS-status indicator, vertically centered; no invented 130px status-text column |
| Action: minimum ~112px, separate `34 × 34` menu | Minimum 112px disabled-demo action, plus user-requested 34px rounded gear action, both centered vertically |
| `QFrame#AIHubModelCard`: 14px corner radius, 1px border, source palette | `HubModelCard` 14px radius, common `Theme` tokens; fixed 84px row as tested in app preview |

The browser/catalog view uses a 0px list spacing with per-visible-item 9px bottom padding. A hidden category's repeater has a 0px-height wrapper *and no padding*, so ASR with two items starts at the exact same top edge as TTS. The card now emits parameterless `open-settings()`; the repeater delegate captures the original full-catalog `index` and calls `open-details(index)`. The earlier callback parameter named `index` shadowed the repeater index and received a hardcoded `0`, so every gear could open the first entry. The new contract removes that ambiguity.

The Slint `HorizontalLayout.alignment: center` aligns along its **main horizontal axis**, unlike Qt's `AlignVCenter`. Using it on a full row clustered the contents in the middle and caused variable left offsets between entries; it must not appear on the root card row. Vertical centering of chips/actions/icons uses nested `VerticalLayout` wrappers while text remains top-aligned.

Local QA: build and run a debug-only Rust/Slint `AppWindow` with the **existing Sidebar** and production `AiHubPage` and `Theme`; only unrelated page imports are omitted in the debug preview. `DISPLAY=:92 SLINT_BACKEND=winit-software`, 1560×918 actual pixels captured using Xvfb/scrot. TTS → ASR and gear navigation exercised via XTest. No Actions/CI were used. Demo model rows exist only in the local debug preview. All edits are within the AI Hub GUI/Theme/icons and documentation; no absolute `x/y` positioning is introduced.

## C4 follow-up: guarded navigation and bounded titles (2026-11-08)

- When a settings draft is dirty, clicking another category stores the requested category until the user explicitly chooses **«Сбросить и выйти»**. **«Остаться»** cancels the pending category change, while **«← К моделям»** cancels any earlier pending category transition and uses the same explicit discard confirmation. No writes are sent to Core.
- Model titles are rendered on one line with ellipsis, and descriptions are bounded to 30px with elision, avoiding vertical overlap and clipped third lines in fixed-height 84px cards.
- The model gear callback carries no integer; the parent repeater supplies the canonical full-list index, including when categories/search/filter hide preceding rows. This guards against selecting entry `0` regardless of clicked row.
- `AppWindow` still creates an empty, read-only `AiHubPage` until a Core-owned presenter feeds its typed catalog. The C4 follow-up does not claim verified install, dynamic status, schema save, or cross-platform parity.

### Executable acceptance in this follow-up

Rust 1.98.1, Slint 1.18.1, Linux Xvfb and `SLINT_BACKEND=winit-software`. The QA binary is a separate local executable that imports the actual `Sidebar`, `AiHubPage`, `HubModelCard`, and `Theme`; only its model fixture is synthetic, outside production.

- `cargo check --locked --offline --features gui --bin neuromita-gui`: PASS on the modified branch with `CARGO_BUILD_JOBS=1`. The container lacks fontconfig/ALSA development `.pc` metadata; temporary local metadata for the already installed runtime libraries was supplied via `PKG_CONFIG_PATH`, without editing vendor or Cargo manifests.
- QA `cargo build --locked --offline`: PASS, renderer launched under Xvfb at 1560×918 and 1180×680.
- XTest gear on **second TTS row** opens the second model's ID and name, not first row. ASR category hides all earlier TTS rows; gear on **second ASR row** opens `Vosk` (`asr-2`) rather than catalog entry zero.
- Editing the first field marks the draft dirty; selecting ASR leaves the TTS settings open with an explicit discard prompt; "Остаться" preserves the draft; "Сбросить и выйти" switches to ASR without persisting the draft.
- A long title and description at 1180px are elided within the 84px row; other cards remain accessible using the vertical scroller.

This is executable QA for the UI components, not a production presenter/runtime E2E, Windows certification, or proof of functional Install/Remove/Save. Screenshot files were generated locally and are not stored in this repository.

## C4 compact navigation and catalog toolbar (2026-11-08)

The category strip is a deliberate user-approved UX departure from PyQt: each tab now aligns its icon to the combined title/subtitle block, drops the decorative bottom line, and shows the **total category inventory count**. The supplied counts are the parallel display projection `category-counts` (`TTS`, `ASR`, `Core`, `RAG`, `Else`); the presenter must compute them from the **same** canonical `entries` snapshot, including the Core/backend and Else/voices/extras/beats/deps category aliases. Counts are not filtered by search or execution backend. No independent persisted count source is introduced.

The former separate title/search and implementation/refresh rows have become one 40px toolbar: category heading, icon-bearing backend selector popup (All, NVIDIA/CUDA, ONNX, CPU), centered search input, and a 36px refresh icon button. The refresh action is **disabled by default**. It becomes enabled only when a real catalog presenter supplies `can-refresh` and handles `refresh-requested`; hovering an unavailable action provides an explanatory tooltip, not a fake success. The selector changes the existing `runtime-filter` only and leaves category counts unchanged.

The shared application Sidebar remains the original component, including Home, Settings, Sandbox, Releases, Gallery, Wiki, Logs, About, AI Hub, social links, and language controls. Its static vertical spacing is compacted so all items remain visible at the minimum 1180×680 window. A previous offscreen QA screenshot showed navigation icons without text because its local harness did not implement the `I18n.lookup` callback, not because production Sidebar lacked entries. The separate QA harness now supplies the actual source-text fallback for that callback. Demo models and their derived category counts live **only** in the local QA binary.

Verified with Rust 1.98.1 / Slint 1.18.1 on Linux: actual shared Sidebar + actual `AiHubPage` in the winit software renderer under Xvfb, screenshots at 1560×918 and 1180×680, category switching TTS → ASR, ONNX popup selection and filtering, and hover tooltip. `cargo check --locked --offline --features gui --bin neuromita-gui` and `cargo fmt --all -- --check` passed on the edited source tree. This does **not** establish production presenter E2E, dynamic inventory wiring, runtime refresh, Windows layout, or installation actions.
