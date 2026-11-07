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
actual filtered list size. Restarting after EULA acceptance does not re-show the
modal; users can reopen the Guide from Settings. The EULA decision and settings
persistence remain unchanged.

**Not implemented:** persistence of `GUIDE_LEVEL`/`INTERFACE_MODE`, per-control
focus rectangles and hotspots, image crop/zoom, direct page-specific local Wiki
navigation, contents menu / section progress, advanced keyboard/accessibility
polish, and Windows GUI behavioral E2E. The Help button still opens the
existing general Wiki URL. Historical screenshots might depict Python UI
rather than the current Slint layout. These limitations are deliberate and
should remain visible until explicit parity acceptance.

Verification: `cargo test --locked --offline --lib onboarding::guide::tests`,
`cargo check --locked --offline --features gui --bin neuromita-gui`, and
format/diff checks. Library unit tests check level membership, unique IDs,
nonempty bilingual content, screenshot assets, fallback locale, and retaining a
visible page across level changes. A successful Linux GUI *compile* does not
certify mouse-driven Slint or Windows behavior.
