# Python Guide → Rust/Slint: page catalog and onboarding navigation

This is a **partial** implementation of issue #41, not full PyQt Guide parity.
The read-only reference is `Atm4x/NeuroMita@5f3d061f` in
`src/ui/widgets/guide_widget.py`, specifically `GuideWidget._init_pages()`.
That method registers **13 pages**; additional Python page classes not registered
there are not included in the Rust page list.

The authoritative page metadata, RU/EN titles, descriptions, image keys and
reference Wiki target are owned by `src/onboarding/guide.rs`. This is not a
second settings database. The old three-picture Slint demo is replaced by
level-filtered pages, selected by UI callbacks that query this Rust catalog.
The original Python `<b>` markup is removed for Slint's plain-text renderer;
wording, numbered controls, and line breaks otherwise follow the referenced
historical Python text. Existing `assets/guide/guide_*.png` are reused.

Levels match Python's cumulative filters:

- Basic: Welcome → Preset → Final (3 pages).
- Advanced: Basic plus Voiceover and Microphone (5 pages).
- Full: all 13 registered pages, including RAG/vector/graph memory, screen,
  camera, models, sandbox, and Final.

Selecting another level attempts to retain the same page, otherwise the closest
preceding page that is visible at the new level. Navigation is bounded by the
actual filtered list size. The chosen level is now persisted as the typed
`onboarding.guide_level` setting through the active SettingsPresenter/runtime
command, not by a second GUI JSON writer. Runtime administration snapshots
synchronize the displayed level. Historical Rust settings without the new field
deserialize as Basic; invalid values fail decoding. The Python settings import
recognizes `GUIDE_LEVEL` as exactly `basic`, `advanced`, or `full`. Old
`INTERFACE_MODE` is intentionally not mirrored into a second guide preference.
Restarting after EULA acceptance does not re-show the modal; users can reopen
the Guide from Settings. The EULA consent field and acceptance flow are unchanged.

**D16 focus implementation:** All 64 original Python normalized focus rectangles
and six screenshot crops are maintained in typed `guide/focus.rs` metadata,
referenced by the canonical Rust page catalog. Slint `GuideMedia` uses native
source clipping, aspect-ratio-preserving size, and correctly aligned overlays
relative to the *cropped* screenshot. Users can select the numbered focus steps
through keyboard-accessible buttons; the selected rectangle is highlighted and
the zoomed screenshot is centered toward it. The image geometry and button list
are not separately hardcoded in Slint.

Section navigation now matches the six historical PyQt groups, with only
groups visible for the current Guide level (Basic two, Advanced three, Full
six). Each section tab jumps to its first page and indicates the current group.
The ordered group ranges and RU/EN labels are computed in the same Rust page
catalog, not duplicated in the UI.

**D17 page navigation:** The `GuideNavigator` component renders one clickable
progress segment for every currently visible page, with done/current/upcoming
states and an accessible page label. A localized Contents popup lists page
titles, section names and numbers; selecting a page navigates through the
existing Guide callback. Both models are derived by the GUI presenter from the
same Rust catalog and reflect guide level and RU/EN language changes. There is
no extra page list or persisted progress state in Slint.

**Guide locale and reopen:** D17 adds RU/EN buttons inside the Guide while
keeping the app's global language setting independent. During a visible Guide
session, `render()` no longer overwrites the guide-local language with the
global locale. Closing the Guide allows its language to resynchronize on
subsequent app render; reopening from the launcher starts at page zero and
clears zoom/focus. Changing the Guide language or level also resets the
selected screenshot step and zoom, mirroring Python `show_page()` behavior.
The EULA consent path still uses its existing language selection and remains
unchanged.

**Not implemented:** rich-text inline step links/hover, full PyQt image toolbar parity (fit-width/actual-pixel-size,
scrollbars and free panning), direct page-specific *local* Wiki navigation,
advanced keyboard/accessibility E2E and Windows GUI behavioral E2E. The Help button now links to the exact historical
RU/EN Wiki article for the current page using the pinned read-only Python
documentation revision; pages without an article hide Help. Centered screenshot
zoom is supported at 1×–2.5× with explicit buttons and clipping; this is not
equivalent to the PyQt per-hotspot focus walkthrough. Historical screenshots
might depict Python UI rather than the current Slint layout. These limitations
remain visible until explicit parity acceptance.

**Required, not yet executed on D15–D17 patches:** `cargo fmt --all -- --check`,
`cargo test --locked --offline --lib onboarding::guide::tests`, `cargo check --locked --offline --features gui --bin neuromita-gui`,
scoped Clippy, and interactive/Windows acceptance. GitHub has no CI status
checks or workflow runs for D16 HEAD. Previous D14 test evidence does not
certify subsequent patches. Library unit tests check level membership, unique IDs,
nonempty bilingual content, screenshot assets, fallback locale, and retaining a
visible page across level changes. A successful Linux GUI *compile* does not
certify mouse-driven Slint or Windows behavior.

D15 persistence checks: historical onboarding JSON compatibility, strict level
round-trips, selected-level restart/EULA independence, presenter-owned mutation
with observable revision, and GUI command/snapshot wiring. Visual acceptance
still requires an adequately provisioned Slint GUI runner.

D16 source reference: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`,
`GuideWidget._STEP_TARGETS`, `IGuidePage.get_image_crop`.
Do not treat these original Python screenshot hotspots as current Rust controls
until interactive GUI acceptance confirms their usefulness.

D17 source reference: `GuideWidget._rebuild_progress_steps` and
`GuideWidget._show_contents_menu` from the pinned Python Guide source.
The Slint popup is scoped to the active Guide screen rather than a
new application-global route or a second settings store.

## D18 — executable compilation gates (2026-11-07)

The source migration is kept inside the existing core-owned Guide catalog and
Slint presentation boundary. The D18 patch updates Slint 1.18.1 image crop
expressions to implicit numeric conversions (Slint does not support Rust's
`as float` / `as int` syntax), preserving all 64 focus targets and six crops.
The GUI argument parser now retains an owned optional explicit directory and
passes `as_deref()` to `PortableDataRoot::resolve()`, then copies the resolved
`root.path()`. Runtime diagnostics and active CLI initialization also use the
canonical portable-root API; these changes do not change EULA acceptance or
settings ownership.

Verification on the **full checked source** with Rust 1.98.1, vendored Cargo
packages and isolated Linux linker/pkg-config shims (outside the repository):

- `cargo fmt --all -- --check`: PASS.
- `git diff --check`: PASS.
- `cargo test --locked --offline --lib onboarding::guide`: 10 PASS.
- `cargo check --locked --offline --features gui --bin neuromita-gui`: PASS.
- `cargo check --locked --offline --bin neuromita`: PASS.
- `CARGO_BUILD_JOBS=1 cargo check --locked --offline --all-targets --features gui`: PASS.
- Full library suite: 395 PASS, one **environment-dependent failure** in
  `ai_runtime::assets::generation::tests::approved_installer_stages_verifies_and_publishes_only_declared_assets`.
  That test explicitly requires Python 3.12, while this runner has Python 3.13.5.
  Repeating with that test explicitly excluded gives 395 PASS, zero failures;
  the excluded case is NOT considered verified **by the local Python 3.13 runner**.
- **Independent Python 3.12 verification:** GitHub Actions run
  [42375125455](https://github.com/Atm4x/neuromita-rust/actions/runs/42375125455)
  checked out exact D18 code SHA `377a54a167c2b1d200164dc8ddf08513bbce1d9b`,
  installed Rust 1.98.1 and Python 3.12, and executed the complete library
  suite **396 passed / 0 failed / 0 skipped**. This closes the missing fixture
  verification without suppressing or modifying its test.
- Full library Clippy with `-D warnings`: FAIL on nine existing issues in
  `src/prompts/{dsl,expression,post_dsl,mod}.rs` and
  `src/request/prompt_preparation.rs`; none occur in the D18 modified source.

These are compilation and source-level proofs, **not** Windows/desktop E2E or
pixel-accurate image focus acceptance. GUI interaction tests, local Wiki
routing and inline rich-text step hotspots remain open. Issue #41 must stay open until those are validated.

## D19 — inline step links and image inspection

The Guide text is now rendered by Slint 1.18.1 `StyledText` with styled markup
prepared from the canonical `GuidePage` descriptions, not from a second list of
instructions. The formatter recognizes the source's numbered `N -` control
markers only when `N` exists in the current page's typed `focus_steps` catalog;
unrelated numbers, non-existent targets and Markdown punctuation are preserved
as regular escaped text. Links have the local `guide-step:N` scheme. The GUI
callback validates that the full scheme and canonical number match a target on
the *currently visible page* before calling the pre-existing focus handler.
Unknown links cannot open external URLs or move the screenshot. The accessible
numbered step chips remain available as an alternative navigation path.

`GuideMedia` supports Fit (contain), Width (fit cropped image width), and 1:1
(original cropped image pixel dimensions at Slint logical-pixel scale) sizing.
The zoom controls run from 1x to 6x relative to the current sizing mode.
When an image is larger than the viewport, pointer dragging pans it with
clamping; selecting a numbered step resets manual pan and centers the selected
focus rectangle. Resizing the window recalculates width/fit sizing. New page,
Guide level, and Guide language changes reset zoom, mode, focus and pan.
Image crop coordinates and all 64 focus rectangles remain in the same Rust
catalog introduced in D16.

The following still require work: transient link-hover focus (Slint `StyledText`
exposes `link-clicked` but no per-link hover callback), actual-size/high-DPI
visual acceptance, pixel-accurate image behavior, local/offline Wiki article
routing (the current `PageId::Wiki` is a placeholder), and Windows GUI E2E.
The existing external historical Wiki URL is not an offline navigation route.
Onboarding consent and the canonical SettingsOwner remain unchanged.
