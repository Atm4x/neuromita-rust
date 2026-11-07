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

The browser/catalog view uses a 0px list spacing with per-visible-item 9px bottom padding. A hidden category's repeater has a 0px-height wrapper *and no padding*, so ASR with two items starts at the exact same top edge as TTS. The `open-settings(index)` callback continues to use the original full-catalog index and therefore selects the correct model when filtering.

The Slint `HorizontalLayout.alignment: center` aligns along its **main horizontal axis**, unlike Qt's `AlignVCenter`. Using it on a full row clustered the contents in the middle and caused variable left offsets between entries; it must not appear on the root card row. Vertical centering of chips/actions/icons uses nested `VerticalLayout` wrappers while text remains top-aligned.

Local QA: build and run a debug-only Rust/Slint `AppWindow` with the **existing Sidebar** and production `AiHubPage` and `Theme`; only unrelated page imports are omitted in the debug preview. `DISPLAY=:92 SLINT_BACKEND=winit-software`, 1560×918 actual pixels captured using Xvfb/scrot. TTS → ASR and gear navigation exercised via XTest. No Actions/CI were used. Demo model rows exist only in the local debug preview. All edits are within the AI Hub GUI/Theme/icons and documentation; no absolute `x/y` positioning is introduced.
