# Beat Sync — Rust core derived-cache slice (#29)

This is a **partial implementation** of issue #29, not full Beat Sync parity.
The current production Python/Unity path and the installed backend set must be
verified at a pinned source revision before connecting a worker to this module.
Source functional audit: [music-beats.md](music-beats.md).

`music::BeatTimeline` models the *result* of an analysis: monotonically ordered
beat times, provider-specific confidence, duration, sample rate, winning method,
and optional BPM. Validation rejects NaN/Infinity, out-of-range confidence,
non-increasing beats, beats beyond duration, and unbounded results.

`music::BeatCache` stores validated analysis results as bounded JSON envelopes.
Identity includes SHA-256 of the audio input bytes, requested backend preference,
backend/model revision, decoder revision, and analysis settings revision. These
revisions must be supplied by the analyzer integration, not guessed from the
installed/selected backend. The actual winning `method` remains on the result.
The consumer may apply `min_confidence` after cache lookup; it is not in the
key because the unfiltered confidence values are retained. Different methods'
confidence scores must not be treated as calibrated against each other.

The caller **must hash the same immutable audio bytes the analyzer uses**.
Hashing a mutable path and later asking a worker to reopen it introduces a
TOCTOU identity race and is not a valid integration. No implicit fallback,
installation, or model readiness is asserted here. Failed cache writes should
not turn an otherwise successful analysis into an analysis failure.

The cache returns `Miss`, `Hit`, or `Corrupt`; malformed entries never become
results. Writes use unique staged files with file sync and rename. This slice
has neither a cross-process writer lease nor directory-fsync durability, so
those aspects require separate review before concurrent production activation.

The initial cache-only paragraph is superseded by the native DSP runtime
implementation and integration status below. The optional Python model workers
and end-user GUI remain distinct future integration work.

## Review update — 2026-11-07

Source checked: `Atm4x/NeuroMita` at
`97310d6aaf6bf7eeb15ae84cf606adc0f024e135`,
including `src/game_connections/services/beat_service.py`,
`src/handlers/ai_engine/services/beats_service.py`, and
`src/game_connections/handlers/actions/get_music_beats.py`.

The Python worker **filters in the backend** before it returns the result:
Beat This emits confidence 1.0, Librosa emits 0.9, and DSP filters
its normalized peak confidence. A production adapter MUST request
`min_confidence=0.0` (and validate that the selected backend returns
unfiltered beats) before storing a timeline. Only a later
`BeatTimeline::with_min_confidence` call may apply the user's
threshold. Using the Python default 0.2 for the worker request would
permanently discard quieter beats from the supposedly reusable cache.

The existing Python `extract_beats_streaming` completes the full
analysis first, then emits one ready chunk. It is not progressive
inference. Rust must not promise partial generation/cancellation based
on this API. Python's backend selection permits fallback; cache
revisions must cover the entire candidate backend/model environment,
not only the configured preference. The final `method` in the result
must be preserved separately.

The on-disk Rust envelope is now format version 2 with a SHA-256 digest
over the serialized timeline. The digest rejects syntactically valid
accidental modifications; it is **not an authenticated MAC** against
a malicious actor who can rewrite the entire cache entry. Format-v1
Rust entries are invalidated instead of silently trusted.

### Independent remaining stages

1. **Source snapshot and invocation contract:** immutable media bytes
   bound to the worker input, real decoder support/limits, bounded
   source reads, source revision and model revision, fixture compatibility
   with the pinned Python implementation, and explicit errors for
   missing/unsupported/corrupt media.
2. **Execution and cancellation:** managed Python worker lifecycle,
   explicit installed/loaded/usable states, real fallback result and
   failure reason, separate cancellation semantics for queued work,
   running inference and completed results; no false promise of killing
   a blocking native inference thread.
3. **End-to-end game contract:** typed requests and
   `music_beats_job_started`, `music_beats_chunk`,
   `music_beats_ready`, `music_beats_error` results. Non-stream ready
   carries beats; current Python streaming emits the full result once
   and then a summary-only ready.
4. **Batch and GUI:** recursive supported-extension discovery, actual
   codec readiness, bounded per-file processing and cancellation,
   cache stats and invalidate/rebuild controls, progress and error
   observability. Preserve Python's build-missing semantics unless
   an explicit rebuild is requested.

Acceptance is **not** complete until real WAV fixtures and replayed
Python-compatible request/response flows are tested. The synthetic
cache fixtures do not establish actual beat detection quality or Unity
delivery. PR #190 should remain Draft until these stages are complete
or explicitly split into follow-up PRs.

### Cache-first producer boundary

`BeatCache::analyze_or_reuse` now hashes the supplied byte slice and
passes that **same immutable slice** to the injected analyzer callback.
It returns the filtered timeline and a typed origin (cache or fresh
analysis), whether a corrupt entry was recomputed, and an optional
nonfatal cache I/O/storage warning. Invalid identity and threshold are
rejected before invoking the producer. This is a synchronous
**integration seam**, not a production worker implementation; callers
must bound source size and offload expensive analysis outside the
real-time Core executor.

The adapter deliberately cannot guarantee that a producer obeys the
raw-confidence contract, nor can it guarantee worker cancellation.
End-to-end parity requires a worker with a verifiable raw-output
contract and real fixture coverage. Source file path → immutable byte
snapshot → actual decoder/worker still needs a dedicated implementation.

## Native DSP runtime and GameLink integration (2026-11-07)

The new `music::wav` parser accepts bounded RIFF/WAVE files with PCM16,
PCM24, PCM32 or IEEE float32 (mono/stereo/multichannel downmix, 1–8 channels,
8–192 kHz). Inputs are capped at 64 MiB and 15 minutes; truncated, unsupported,
invalid samples, oversized audio and cancellation are explicit errors.
The native `dsp_energy_onset_v1` algorithm detects energy transients with
10-ms windows and a minimum beat separation, then estimates BPM from the
median interval. It is a **new deliberately named fallback**, not a
numerically equivalent port of Python spectral-flux DSP or Beat This.

`BeatRuntime` now provides one immutable-source read, cache-first analysis,
post-analysis confidence filtering, optional game event emission, and bounded
recursive WAV-only build-missing indexing (10,000 files, 16 levels).
File hashes, decoder and analyzer revision form the content-addressed cache
identity. The `beat_sync` binary provides a direct reproducible smoke route:

```text
cargo run --locked --offline --bin beat_sync -- /absolute/path/track.wav /absolute/path/cache
```

GameLink is opt-in via `ApplicationConfig::game_beats` with a
`GameBeatConfig { media_root, cache_root, enabled }`. Without an explicit
config it is **disabled**. The root is canonicalized at binding; the GameLink
command accepts only WAV files whose canonical resolved path is inside this
root. At most two parallel jobs are admitted; each carries a cancellation
token and an owned game-session request ID. The job runs off the async
transport executor. A session can ask for:

```json
{"command_id":"<uuid>","command":"get_music_beat_status"}
{"command_id":"<uuid>","command":"get_music_beats","request_id":"<uuid>","audio_path":"track.wav","track_name":"track","stream":true,"min_confidence":0.2,"backend":"dsp"}
{"command_id":"<uuid>","command":"cancel_music_beats","request_id":"<uuid>"}
```

The accepted request emits `music_beats_accepted` (command receipt),
then `music_beats_job_started` and either a `music_beats_ready`
with beats when `stream=false`, or one `music_beats_chunk`
and summary-only `music_beats_ready` when `stream=true`.
Failures emit `music_beats_error`; malformed/rejected requests use the
existing GameLink `error` response. The transport does not pretend that
the single completed chunk represents progressive decoding or inference.

Readiness reports only DSP as available. `BeatThis` and `Librosa`
preferences are explicitly rejected rather than silently pretending a
model was initialized. `Auto` uses the native DSP implementation as
the only available fallback. Model installation, GPU readiness, external
Python worker activation, non-WAV codec support, interactive GUI cache
management, and verified live Unity animations are NOT included in this
native fallback scope.

### Validation and residual risks

`tests/beat_sync_runtime.rs` generates actual WAV fixtures and checks
tempo/onsets, cache reuse, error/cancel, source snapshot behavior, streaming
event payloads and batch indexing. `tests/game_beat_sync.rs` exercises
a real GameLink TCP socket (hello, status, request, ready), and rejects a
path traversal attempt. The CI workflow runs these plus the existing
cache tests, all-target strict Clippy, rustfmt and the full offline
Rust suite with Python 3.12 available.

This still does **not** establish parity with Python's BeatThis/Librosa
models, musical quality on real-world tracks, or Unity visual behavior.
Long audio and hostile concurrent file replacement require dedicated
integration tests. The GameLink adapter validates path scope but does not
provide a sandbox against a malicious *local process* racing file changes;
the local game transport is restricted to loopback. Source snapshots and
worker cancellation are bounded, but an external model worker is not yet
connected.

Do not close #29 as fully completed based on this native DSP milestone alone.
