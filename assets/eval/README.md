# VAD evaluation corpus

18 коротких клипов 16 кГц моно PCM16 для офлайн A/B (`examples/vad_ab.rs`).

- `ru-01..06.wav`, `en-01..06.wav` — google/fleurs, split `validation`, лицензия
  CC BY 4.0. По шесть клипов 4–10 секунд на язык; первые три в манифесте помечены
  `calibration`, остальные `heldout`.
- `noise-01..06.wav` — ESC-10 (подмножество karolpiczak/ESC-50), лицензия CC BY 3.0:
  dog, rooster, rain, sea waves, crackling fire, clock tick.

`sources.json` хранит исходные id, транскрипты, размер и SHA-256 каждого файла.

## Annotation status and strict evaluation

The twelve speech clips use `annotation_status: "heuristic"`: their intervals
come from an energy detector, not manual listening. The six ESC noise clips use
`unknown`; absence of speech has not been verified. No manual labels were added.
`provisional_intervals` in sources.json is provenance; the evaluator reads status
from the manifest. The original manifest is preserved byte-for-byte in
`manifest-heuristic-v1.json`; without status the new evaluator treats it as unknown.

- `unknown` (also a missing status): suppress quality metrics even if speech
  intervals exist. Missing speech labels never prove silence.
- `heuristic`: explicitly provisional quality metrics; speech must be present.
- `manual`: the entire clip has been manually checked; speech is required,
  and an explicit empty array means no speech.
- `verified_noise`: manually verified absence of speech; requires speech: [].

Intervals are half-open sample ranges, ordered without overlaps and bounded by
actual PCM length, including intervals on unknown fixtures. WAVs are limited to
30 seconds and 960000 + 65536 total bytes (headers/metadata included); manifests
to 1 MiB. Reads stop at the byte limit even if the file grows during reading.

```sh
cargo run --locked --offline --example vad_ab -- assets/eval/manifest.json direct 0.5
cargo run --locked --offline --example vad_ab -- manual-manifest.json direct 0.5 --validated-only
```

`--validated-only` rejects the entire manifest if any fixture is neither manual
nor verified_noise, before constructing score providers. The current corpus is
expected to fail strict mode. Thresholds remain explicit, never tuned here.
Wavekat requires vad-wavekat; Silero requires vad-silero and explicit local
model/ORT DLL paths. Provider factories return Result, including Silero creation;
construction errors propagate without expect/panic.

## Metric semantics

Frame precision/recall/F1 use exact overlap durations in samples, rather than
provider-window midpoints or counts. Both 256/512 windows therefore use the same
units. False positive seconds derive from false_positive_samples. Reports expose
scored_samples and unscored_tail_samples: a partial provider window is not padded
or counted as false negative. endpoint_unprocessed_tail_samples separately
records the remainder smaller than one 256-sample capture frame.

Boundary reports distinguish covered (any overlap), matched (one-to-one overlap
only), missed, spurious, merged (predictions overlapping multiple annotations),
and split (annotations overlapping multiple predictions). These are overlap
counts, not an IoU acceptance criterion for correct utterance detection. Boundary
errors use only one-to-one pairs; without pairs the mean is null, not zero.
audio_start_error_ms and audio_end_error_ms are signed: negative means early PCM
boundary and positive means late. PCM start with pre-roll is not detection time.

Endpoints consume SpeechSegment metadata directly, without consumed-minus-length
reconstruction. emitted_segments retains all absolute start/end samples, optional
onset decision, emission timestamp and typed end reason (silence/maximum/release/eof).
onset_decision_delay_ms compares onset_decided_at_sample against annotation start;
PTT has no onset decision. silence_emission_delay_ms compares emitted_at_sample
against annotation end only for Silence; Maximum/Release are forced cuts.
eof_censored_ends counts EOF segments separately; their end errors appear in
eof_censored_audio_end_error_ms and are excluded from ordinary audio end errors.
Timing means exclude ambiguous merge/split pairs. These are stream sample times,
not wall-clock inference or ASR benchmarks. Unknown fixtures still report raw
emitted segments and processing coverage, but metrics/boundary/endpoint are null.

Historical A/B reports use different formulas and cannot establish backend
acceptance. A new comparison needs a validated corpus and a fresh run.

The six unknown noise clips are excluded from all quality metrics, while every
clip retains its raw emitted segment metadata in the report. Previous noise-quality
numbers cannot be reproduced as verified quality measurements until those clips
are listened to and explicitly annotated as manual or verified_noise. Category
names alone do not establish absence of speech.

Signed PCM boundary errors describe early/late saved audio, not detection latency.
Decision and emission delays use the corresponding actual event timestamps;
pre-roll cannot be interpreted as an earlier decision. These differences remain
signed rather than clamped to zero. Endpoint evaluation rejects PushToTalk mode:
the offline harness supplies no button events, so it cannot evaluate PTT behavior.
