# Review of concurrent branches and PRs, 2026-11-03

Baseline: `origin/main` `63a565ee` (our PRs #71–73). Review branch: `codex/review-integration-20261103`. Original branch and PR heads remain available. Python 3.12, static APIs, one owned Core, character-owned state, canonical Timeline facts and functional presenters remain the acceptance criteria. Git merge conflicts are not themselves architectural defects.

## Review decisions in progress

| Change | Purpose | Decision / remaining check |
|---|---|---|
| PR #64 `21e89095` | Correct Windows Python fixture selection | Include; test-only correction, explicit environment override retained. |
| PR #65 `51fba543` | Preserve canonical message ID when TTS fails | Include; text acceptance already occurred, receipt must retain its identity. |
| PR #66 `bd7d24df` | Retire an IPC worker when a request future is dropped | Include regression. The newer Core already has `InFlightRequest`; preserve that single guard rather than nesting two equivalent guards. |
| AI `1a0dfdfa` | Managed Python production installation, model source catalog, unified backends, PCM speech sessions and chunked WAV IPC | Include after fixing PCM read cancellation and Windows fixture portability; no live model installation claimed. |
| Lexical memory `424ff421` | Explicit Russian lexical dedup operation | Review/integration pending; ordinary Add and semantic retrieval must remain independent. |
| Character GUI `ab1eca00` | Qt-style character library, accordions and inline memory | Include after fixing atomic preset assignment and confirmed split/common mode. |
| Releases/About `977e6044` | Implement the release feed and About views | UI layouts are usable; move HTTP/work ownership out of the Slint binding before including the feature. |
| PR #67 `78a482d3` | Request diagnostics and opt-in training dataset capture/export | Keep separate as written: `MessageHandler` awaits dataset persistence before text-ready, cancellation completion and finished; store uses blocking cross-process file locking. Auxiliary storage can stall the request lifecycle and shutdown. |
| PR #68 `88b75311` | Sea Battle rules, model actions and automatic reactions | Keep separate pending contract decision: authoritative match is volatile; historical generation is rejected while a match is active. This changes the existing historical context guarantee. Atomic move preparation/privacy tests are useful, but do not resolve that limitation. |
| PR #69 `5a8771f6` | Persistent reminders/timers, timezone/DST and scheduler delivery | Review/integration pending; inspect queue bounds, lifecycle and atomic acceptance before deciding. |
| PR #70 `42c9a026` | Hardware/device selection, execution probe and staged activation | Keep draft: explicitly untested WIP and based on older worker/composition contracts; reconcile with the new unified backend installer before acceptance. |

## GUI responsibility

Our earlier `memory.slint` was an independent modal editor. Its CharacterHandler/CAS data boundary was valid, but the separate modal did not follow the requested character settings composition. The user's newer branch turns it into an inline `MemoryPanel` inside the character history/memory accordion, with Sandbox navigating to that section. Preserve that composition; the file's name/location does not make memory a separate owner or page presenter.

The Qt branch currently sends three independent preset assignments from Slint and keeps the split/common flag only in frontend globals. A failure or external settings edit can leave the visible mode inconsistent. Replace that command group with one typed settings edit and derive the mode from confirmed assignments.

## Verification record

- Initial AI merge: Python SDK 43 passed after replacing a POSIX-only fixture path assertion with native path components.
- Interrupted PCM read regression: passed with synthetic bytes; frame data survives cancellation in the middle of a sample.
- PR #66 regression: passed against the current worker launcher after adapting the fixture to explicit model configuration.
- Full combined checks and final GUI build: pending.

Transport-only `bridge/*` branches contain export/import workflows and encoded bundles. They are provenance/delivery mechanisms, not additional application features. Do not copy temporary payloads/workflows into the product tree merely to make every ref appear merged.
