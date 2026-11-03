# Issue 60 first increment: stream tickets and score framing

This increment is ready for parent review/publication to draft 77 after the checks below. It does not close issue 60. No Cargo manifest/lock/config/vendor change, audio fixture acquisition, model download, microphone capture, GUI change or physical hotplug acceptance is included.

## Result

The existing speech dispatch queue now carries immutable client/character routing, stream/generation identity and an utterance UUID. Reset/new PTT press cancels the generation shared by active and queued work. Stale work is checked before ASR, after ASR and at synchronous chat admission. Admission is once per ticket and serialized with generation reset. ChatReplyTarget captures the original ChatApiState rather than resolving the global current Core at delivery. The production Application factory binds its own owner explicitly. An actual Core A shutdown / Core B startup test verifies that an old target cannot publish into B.

Stream lifetime cancellation recovers a poisoned mutex without panicking during unwind. This prevents a panic in the public synchronous admission callback from causing a second panic in Drop. No async work occurs under admission/generation locks.

The segmenter retains at least the onset confirmation window even when configured pre-roll is shorter or zero; onset larger than maximum segment capacity is rejected. Marker-based regression proves no dropped/duplicated frames through continuous-voice maximum-duration splits once onset is confirmed.

The score framework remains independent of wavekat/ONNX dependencies. It supports bounded chunk assembly for 256/512-sample providers, records sample offsets, reports an unpadded tail and resets all framing/provider state between streams. Provider/probability/offset failure poisons the stream until explicit reset; a failed chunk returns no partial score vector and must not contribute success metrics. Direct Earshot rejects 255/257-sample frames explicitly. A bounded local Silero asset reader checks the pinned v6.2.3 size/hash, but no local asset or real Silero inference has yet been exercised.

Ticket UUID/once-admit is not global acoustic/transcript deduplication. Repeated PCM with a new ticket remains a separate utterance. Reset cannot undo synchronous admission completed before reset acquired the lock.

## Validation evidence

All Cargo commands used `--locked --offline`, an absolute source override to `C:/Development/NeuromitaRust/.worktrees/issue-51-current/vendor`, shared `C:/Development/NeuromitaRust/target`, debug0/incremental0, NASM PATH and serial tests (`--test-threads=1`). Python was explicitly `C:/Users/Dmitry/AppData/Local/Programs/Python/Python312/python.exe` (3.12.7); NumPy cache import was verified as 1.26.0. Python SDK files were not changed.

- Full default suite: **438 passed, 0 failed, 2 ignored**. Log: `C:/Development/NeuromitaRust/target/issue-60-default-final.log`.
- Full no-default suite: **377 passed, 0 failed, 2 ignored**. Log: `C:/Development/NeuromitaRust/target/issue-60-no-default-final.log`.
- After the final narrow Drop/type/lint fixes: focused `--lib audio::`: **21 passed, 0 failed**; focused `--test audio_pipeline`: **1 passed, 0 failed**. Logs: `issue-60-focused-audio-final.log`, `issue-60-focused-pipeline-final.log` under the same target directory. Full suites were not repeated after these reviewed narrow fixes; the focused runs cover them.
- Final all-target Clippy default/no-default with `-D warnings`: **PASS**. `cargo fmt --all -- --check` and `git diff --check`: **PASS**. Logs: `issue-60-clippy-default-final.log`, `issue-60-clippy-no-default-final.log`, `issue-60-fmt-final.log` under the same target directory (fmt is silent on success).

An initial default run omitted NEUROMITA_TEST_PYTHON and failed with Spawn OS error 1920 on the python3 fallback. Explicit Python312 targeted retry and both subsequent full suites passed; no source workaround or timeout increase was made. A score test expectation was updated to ResetRequired after poison behavior was introduced. Clippy's inferred slice conversion finding was fixed with an explicit fixed-size array reference, not by removing validation.

## Pending acceptance

Wavekat Earshot and optional Silero still require a separately reviewed dependency increment and real RU/EN/noise A/B. The direct dependency proposal is in issue-60-vad-dependencies.md; parent approved resolution only after this tested commit, with no build/vendor and a 30 MB source budget. Resolution must report exact package versions/checksums/Rust compatibility before vendoring.

Independent source review found no additional introduced blocker, but the preexisting partial-PCM-before-gating edge remains pending. Complete radio/client gate parity, speaking lease, bounded EOF drain, device rebind/hotplug and applicable GUI state also remain pending; none is claimed fixed by these queue tests. Physical capture requires explicit user action. Reference speech boundaries require independent annotations; transcripts alone do not justify F1/boundary-loss metrics. No real-microphone, real-ASR-model, GPU or full issue acceptance is claimed.
