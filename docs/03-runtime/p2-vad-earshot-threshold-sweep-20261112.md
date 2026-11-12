# Earshot threshold sweep — P2 diagnostic, 2026-11-12

Status: **R&D measurement only**. This is not a production threshold change, full Silero A/B acceptance, or evidence of microphone/ASR performance.

## Reproduction

Rust 1.98.1, offline Git bundle of NeuroMita with product Beat Sync #430 and Core-owning VAD evaluator #432 sources applied. CLI uses `SpeechSegmenter<EarshotVad>` with unchanged `SegmentConfig::default()`:

```bash
for threshold in 0.35 0.40 0.50 0.55 0.60 0.65 0.70; do
  cargo run --locked --offline --example vad_core_eval -- \
    assets/eval/manifest.json "$threshold" --validated-only > "vad-$threshold.json"
done
```

All seven JSON runs completed using 18/18 validated WAVs. The reported F1 is **sample-level, direct Earshot score classification**, not word error rate, onset latency, ASR readiness, or inference success. Noise false-positive duration is the sum of direct-classification FP samples divided by 16 kHz. Segment count is the production segmenter's emitted objects for the six noise WAVs, which is not identical to direct FP seconds.

| Threshold | Heldout RU F1 | Heldout EN F1 | Heldout noise FP seconds | Noise segments |
|---:|---:|---:|---:|---:|
| 0.35 | 0.939 | 0.805 | 6.53 | 10 |
| 0.40 | 0.953 | 0.822 | 5.63 | 9 |
| 0.50 | 0.955 | 0.795 | 4.32 | 9 |
| 0.55 | 0.956 | 0.779 | 3.74 | 7 |
| 0.60 | 0.956 | 0.743 | 3.06 | 5 |
| 0.65 | 0.951 | 0.711 | 2.42 | 4 |
| 0.70 | 0.939 | 0.648 | 1.89 | 2 |

## Statistical boundary and decision

Manifest role audit: RU and EN each contain **3 calibration and 3 heldout** manually annotated clips; **all 6 noise clips are heldout**, none noise-calibration. The sweep above *looks at heldout outcomes*. It must not be used to select and then claim an unbiased optimal threshold on the same heldout clips. These are small, heterogeneous samples with no repeated-speaker or real microphone coverage.

Increasing threshold from 0.50 to 0.70 reduces noise FP by 2.43s, but the heldout EN F1 falls by 0.147. A global high threshold is therefore **not a demonstrated improvement**, and could suppress English speech. No threshold is automatically promoted to Core. A defensible next step is an independent, manually annotated noise-calibration subset from new recordings, a separately sealed test set, and Pareto evaluation of **false speech admissions versus missed RU/EN utterances, per speaker/microphone**. Compare optional Silero ONNX with verified local assets and identical framing/gating rules before selecting providers.

## Separate verification

The product Beat Sync source port and evaluator compile in the same **older local bundle** with Rust 1.98.1: `cargo fmt --all -- --check` PASS, `cargo check --locked --offline --all-targets` PASS; Beat cache 11 PASS, Beat runtime 8 PASS, GameLink 1 PASS, audio pipeline 1 PASS. They do **not** constitute exact GitHub `main=8230fa25...` merged-tree CI. PR #430 exact-main workflow [43096939388](https://github.com/Atm4x/neuromita-rust/actions/runs/43096939388) remained **queued without jobs** at this checkpoint, so both #430 and #432 remain draft and not merged.

Source annotation/provenance: `assets/eval/manifest.json`, `assets/eval/sources.json`, `assets/eval/README.md`. Respect dataset attribution and licenses.
