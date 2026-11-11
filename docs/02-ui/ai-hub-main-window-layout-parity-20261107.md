# AI Hub: main-window layout parity QA (2026-11-07)

## Scope

The reference is the current PyQt AI Hub (`src/ui/windows/ai_hub/dialog.py`, `widgets.py`, `settings_panel.py`, `constants.py`, `src/styles/ai_hub_styles.py` in Python NeuroMita), not the previous isolated Slint crop. The user-authorized adaptations remain top category tabs and a model-specific settings subpage, reached via a square settings button. This change is Slint GUI-only: no presenter, IPC, installation, removal or settings persistence.

## Layout corrections

- Built and ran a **local Rust 1.98.1 binary** with the real production `Sidebar` source, production `AiHubPage` and `Theme` in the same `HorizontalLayout` as `AppWindow`. Only unrelated page modules are excluded by the **separate local debug build**, which is not committed. No GitHub Actions run was used.
- Made the root shell/layout fill the window. Prevented the content panel from inheriting the narrow `600px` preferred-size cap. QA logs verify `window=1560`, `shell=1560`, `sidebar=230`, `content=1330`, `hub=1330` logical pixels.
- PyQt `ModelCard` maps to compact, **84px** tall Slint rows, with 18px left inset, 14px title, 11px descriptive text, one nonwrapping chip row and inline status/action/34px gear. No two-row metadata. Source PyQt uses 18px/14px card insets, a single `QHBoxLayout`, 14px title, 11px description, backend/VRAM/language/size chips and a 34px menu button.
- Active category selection uses a five-way, equal-width tab rail with source-like muted icons and an accent selection surface/underline; tab icon+label are layout-grouped, not positioned with `x`/`y` offsets. These tabs are the user-authorized replacement for the PyQt category sidebar.
- Search is a horizontal icon + text field aligned to the left, filters share the toolbar row, and status dot/text+card gear are vertically centered via layout containers.
- Removed conditional-card height leaks. Nonmatching cards have a zero-height layout delegate, so a category with only two entries starts **at the top**, not vertically centered below hidden categories.
- The footer retains the PyQt four-stat concept. Per-model settings retain a nested page with name, description, status, disk location and fields. The fields are visibly demonstrational and never save data.
- No added coordinates (`x/y`), negative margins, fake absolute icon overlays or new one-off accent HEX colors. AI Hub surfaces derive from the existing `Theme.background`, `Theme.panel`, `.border`, `.text`, `.muted`, `.accent`, `.accent-hover`, `.accent-border`, and `with-alpha(...)` of those base colors. The only common status colors are reusable `Theme.success` / `Theme.danger`. Custom decorations are not a separately maintained AI Hub palette.

## Local compilation and screenshots

The local debug build compiles the actual `Sidebar` and `AiHubPage` Slint source, not a replacement for either component; only unrelated pages are omitted from the temporary root view to reduce memory and build time. Rust `1.98.1`, vendored Slint `1.18.1`, `cargo build --offline --locked` and `Xvfb :92` with `SLINT_BACKEND=winit-software` were used. X11 XTest interactions exercised the selected model settings gear and TTS → ASR category changes.

Screen captures are real 1600×960 framebuffer PNGs in `/mnt/data/agent_c_c4_repair/`, with the full 1560×918 main window and 230px sidebar:

- `main-ai-hub-tts-final.png`: TTS list, compact cards, single-row chips, full-width tabs
- `main-ai-hub-settings-final.png`: per-model Edge TTS + RVC settings subpage
- `main-ai-hub-asr-collapsed.png`: two ASR models directly beneath the toolbar, no empty slots

These local screenshot files are not distributed inside the Git repository. The debug model catalog exists only in the temporary local harness. Production `AiHubPage` still accepts a typed `HubModel[]` from future presenter integration and otherwise displays a truthful empty state.

## Still outside scope

No presenter, dynamic model statuses, backend invocation, real schema-driven forms, install/Remove, or cross-platform visual certification. C4 remains GUI-only, not end-to-end complete.
