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
- [x] Push/merge integration PR #71 into main (`302b98ce`). Subsequently integrate the new idempotent-memory branch (`ab46d9d5`) and its bridge history in the desktop settings branch.
- [x] Replace SandboxPresenter with functional ConversationPresenter; keep Characters/Settings/Presets/Chat and Connections presenters independent of page nesting.
- [x] Start Core/admin/history without a configured LLM; missing preset/credential remains a typed generation error.
- [x] Implement Settings page from Python Qt/QSS: preset CRUD/active selection, three typed per-character preset slots, voice flags/ID/volume and confirmed persistence state. Provider credentials remain environment references.
- [x] Connect history, guarded clear/reset confirmation, retained request diagnostics and live client count/server status. Add memory editor using CharactersPresenter snapshot/CAS, live observer, CRUD/forget/restore/protection, search and paging. External updates do not replace the draft or its edit token.
- [ ] Remaining: message-specific revise/restore controls, character parameter editing, AI model/install UI and actual microphone/capture/playback integration. Disabled controls must not imply those features work.
- [ ] Verify GUI in wide/compact sizes, errors/disconnected states, and real presenter interactions with local fixtures.
- [ ] Commit/push completed increments and keep handoff current before stopping near the allowance threshold.

Earshot reference: https://github.com/pykeio/earshot ; pinned crate 1.2.2 (`c400f15bd232dda30a297eec90235bd09066cab7`). It processes 256-sample / 16 ms frames of 16 kHz mono PCM16 in the native adapter. No microphone capture, ASR, speech-accuracy claim or model download follows from adding this component.

## Current desktop contracts

`ApplicationLifecycle` constructs mechanism-oriented presenters. The Slint bridge sends bounded typed commands to one Tokio desktop task; model/settings/request changes return through the Slint event loop. No page presenter or UI-owned canonical state was introduced. Slint globals hold view/draft data only. `ConversationChangePreview` is opaque and bound to character, branch selection and head; applying an outdated confirmation is rejected. Clear emits MessageHidden facts; context reset emits CharacterContextReset, not destructive deletion or a reset of all character parameters.

The memory bridge owns a current read snapshot and a separate immutable draft snapshot. Re-rendering live memory updates changes only the read list; save uses the draft token and CharacterHandler performs CAS. Memory observation runs in an owned abort-on-drop task so a competing desktop command cannot cancel an in-flight notification read. Pages/search are projections of one captured read snapshot, not another memory store. The native Earshot adapter is still independent of microphone capture.

Settings UI covers the implemented Rust fields, not all Python settings. Source references: `src/ui/pages/settings/settings_page_widget.py`, `src/ui/settings/api_settings/ui.py`, `src/ui/settings/character_settings/ui.py`, `src/styles/pages/settings.py`, theme/base QSS. Existing shared controls/fonts/assets supply the appearance; the volume slider uses the same pink/white palette and 5 px track / 18 px handle. Existing unsupported AI/capture switches remain disabled. No real Gemini inference, voice inference or recording is part of these UI checks.
