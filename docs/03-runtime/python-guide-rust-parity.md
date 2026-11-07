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

**Not implemented:** rich-text inline step links/hover, PyQt page-by-page
progress segments and contents popup, full PyQt image toolbar parity (fit-width/actual-pixel-size,
scrollbars and free panning), direct page-specific *local* Wiki navigation,
advanced keyboard/accessibility E2E and Windows GUI behavioral E2E. The Help button now links to the exact historical
RU/EN Wiki article for the current page using the pinned read-only Python
documentation revision; pages without an article hide Help. Centered screenshot
zoom is supported at 1×–2.5× with explicit buttons and clipping; this is not
equivalent to the PyQt per-hotspot focus walkthrough. Historical screenshots
might depict Python UI rather than the current Slint layout. These limitations
remain visible until explicit parity acceptance.

Verification: `cargo test --locked --offline --lib onboarding::guide::tests`,
`cargo check --locked --offline --features gui --bin neuromita-gui`, and
format/diff checks. Library unit tests check level membership, unique IDs,
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
