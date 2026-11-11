# Core-owned Earshot recorded-audio evaluator

This evaluator reuses the **existing** production `audio::segment::SpeechSegmenter<EarshotVad>` and separately records raw direct Earshot frame scores for comparison with manual labels. It does not install audio devices, open microphones, contact Python/Unity, or create a second policy owner.

`examples/vad_core_eval.rs` takes `<manifest.json> <threshold> [--validated-only]` and emits JSON. Scored frame precision/recall/F1 count sample-weighted overlaps against manually reviewed labels; the 18 WAV files retain their hashes and original attribution in `sources.json` (FLEURS CC BY 4.0, ESC noise CC BY 3.0). Calibration and heldout roles are not silently combined by a threshold tuner; all reported results use the explicit CLI threshold.

The existing production `SpeechSegment` does **not** expose onset-decision samples, silence-vs-max-vs-EOF terminal reasons, or absolute start/end offsets. The adapter reconstructs a segment's observed sample span from the PCM length and frame emission position; it explicitly emits `null` for onset-decision time and terminal reason, and does **not** report silence endpoint latency, censored-EOF timing, or end-error aggregates. These fields are **not** interchangeable with the richer #79 research branch evaluation. They require new metadata from Core to be valid.

`--validated-only` rejects unreviewed/heuristic label manifests before audio evaluation. The evaluator cannot prove no missed words, high-quality real microphone capture, live ASR, optional Silero ONNX readiness, or GUI/Unity integration. The 18 WAVs provide only a small research corpus and a reproducible offline regression baseline.
