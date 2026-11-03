# Desktop implementation progress, 2026-11-03

User scope: finish functional presenter connections, implement existing settings (characters/presets and related sections) from the Python widgets/QSS, integrate all GitHub work, add/vend Earshot and smoke-test without private recordings. Work until the shared 5-hour allowance approaches 6% remaining. Do not ask questions or use subagents. Check actual usage with `get_usage_limits`; initial read: 25% used / 75% remaining.

Presentation contracts are grouped by **mechanism**, not by GUI page hierarchy. GUI and a future TUI consume the same command/read/observe interfaces. Pages only compose those mechanisms.

Python is pinned to **3.12** per the user's clarification. The incoming application lifecycle tests assumed 3.13.5; fixtures now require a selected 3.12 interpreter and derive its exact patch version for the lock. The production interpreter-versus-lock check remains strict. Negative ABI tests use an explicitly incompatible version.

- [x] Fetch all remote branches.
- [x] Merge AI install/activation, application lifecycle, ASR/local embeddings/reranking (`ec9c219f`), retaining GUI additions and both goal sections.
- [x] Merge memory inspection/observer/capacity chain (`3035ed95`).
- [x] Record all bridge histories without transient bundle payloads/export jobs. The GigaAM reference branch is intentionally not an implementation override: the integrated AI branch forbids implicit downloads and uses its own canonical native paths, while the reference has the older Python-only download/helper behavior.
- [x] Add Earshot 1.2.2 through Cargo, vendoring its crate without rewriting existing third-party files.
- [x] Run `cargo run --locked --offline --example vad_smoke`: silence/synthetic sine frames, finite bounded probabilities, deterministic reset and invalid threshold rejection. No private recordings or upstream audio tests. Keep original licenses in `licenses/earshot/`; vendored bytes/checksums are intact.
- [x] Validate merged contract suites on Windows: Rust GUI 356 passed / default 355 passed (2 ignored each), Python 33 passed, GUI all-target Clippy with warnings denied and fmt passed. The pinned-Python fixture was rechecked separately: 8 passed.
- [ ] Push and merge an integration PR into main before continuing presentation changes.
- [ ] Replace page-named SandboxPresenter coupling with mechanism-oriented presentation contracts.
- [ ] Bring up settings and presets independently of a configured LLM; no hidden model/preset fallback.
- [ ] Implement Settings page from Python Qt source/QSS: existing character assignments, preset catalog CRUD/selection, voice settings and actual persistence state.
- [ ] Connect remaining supported Sandbox administration actions through functional subpresenters, including history/reset/message actions, diagnostics, memory and real settings bindings.
- [ ] Verify GUI in wide/compact sizes, errors/disconnected states, and real presenter interactions with local fixtures.
- [ ] Commit/push completed increments and keep handoff current before stopping near the allowance threshold.

Earshot reference: https://github.com/pykeio/earshot ; pinned crate 1.2.2 (`c400f15bd232dda30a297eec90235bd09066cab7`). It processes 256-sample / 16 ms frames of 16 kHz mono PCM16 in the native adapter. No microphone capture, ASR, speech-accuracy claim or model download follows from adding this component.
