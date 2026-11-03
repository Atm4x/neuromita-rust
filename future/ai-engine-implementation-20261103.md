# AI Engine implementation increment — 2026-11-03

Baseline: `codex/slint-ai-runtime-formal-20261103` at `2cd201dc349595833f8a440f2a0f7848a9d2278a`.

This increment changes runtime code and tests, not merely the design. It is **not** a claim of production inference, complete AI Hub UI, or closure of issues #24/#26/#37/#42/#51/#59/#61.

## Implemented and locally verified

- Backend and model-specific overlay installation now verify installed `*.dist-info/METADATA` identities against their exact resolved locks, both before publishing and when reopening immutable revisions. A receipt alone is insufficient to claim readiness.
- The 12 built-in Python TTS descriptors explicitly declare compatible CPython minors (3.11/3.12) for the current NumPy 1.26.0 / SciPy 1.12.0 pins, along with canonical CPU/CUDA/ONNX backend choices. The installer rejects incompatible selected Python versions/backends rather than attempting an installation that cannot satisfy the declared contract.
- The common SDK implements validation of typed settings and bounded enumerations, and a host method for side-effect-free config validation. The runtime can expose model-dependent Silero speaker options *after* model initialization, while Silero and RVC have independent `silero.speaker_id` and `rvc.pitch_shift_semitones` keys.
- Explicit, allowlisted installer actions are represented as serializable data; the built-in Fish Speech install plan uses an explicit `fish_project_root` action. Rust only applies the `create_empty_marker` action to a verified, staged package after exact ID approval, and verifies the result when reopening the revision. Arbitrary Python callables are not transferred over IPC.
- A new `ModelAssetStore` stages user-authorized local/HTTPS assets with explicit SHA-256, bounded transfers and symlink/path checks, and atomically publishes immutable, named plugin revisions. Existing revisions are rehashed on selection; changed/corrupt revisions are rejected.
- A separate `VoiceProfileStore` validates profile identity, settings, referenced WAV/TXT file paths and SHA-256, and resolves references without embedding weights in profiles. `PythonVoiceProvider` maps per-voice settings and references to typed Python inference input and rejects an active model/profile revision mismatch.
- `AiSupervisor` checks named asset revisions before worker activation and passes selected immutable revision paths to the Python process. Rust→Python mock TTS→WAV integration exercises the voice profile and conversion of Rust volume 0–100 into Python gain 0–1. The Python runtime accepts per-request settings across Silero, F5, Fish Speech and OmniVoice.

## Test evidence

- Rust 1.98.1, `cargo test --locked --offline --no-default-features`: PASS (all executed tests; no real model weights).
- Rust 1.98.1, `cargo clippy --locked --offline --no-default-features --all-targets -- -D warnings`: PASS.
- `cargo fmt --all -- --check`: PASS.
- Python 3.13.5 used for **mock protocol tests only**: `PYTHONPATH=python python3 -m unittest discover -s python/tests`: 15 PASS.
- Rust local AI-specific tests: 32 PASS (scoped `--lib ai_runtime`).

## Remaining blockers and explicit non-goals of this increment

1. **Production environment composition**: bootstrap of verified `uv`, Python interpreter, SDK distribution, backend layers and overlays must be assembled into one owned application-startup flow, including authoritative lifecycle and failure recovery. The default `ApplicationConfig` still injects `UnavailableVoiceProvider`; application settings and Slint AI Hub cannot yet select/activate a real installed model.
2. **Real assets**: no trusted model download manifests with pinned hashes have been provided. The new store requires concrete source files/URLs and pinned checksums; it deliberately does not guess sources or automatically download arbitrary third-party weights. The 12 model install descriptors still contain many unpinned assets.
3. **Production inference**: no real PyTorch/ONNX/CUDA model weights, speaker lists or audio quality tests were available; F5, Fish, OmniVoice and Silero/RVC runtime code is structurally connected but not certified by actual synthesis. In particular, the backend package recipe must be tested against the real platforms and Python 3.12 environment.
4. **Streaming/cancel**: TTS currently uses bounded Base64 WAV over JSON-lines IPC. Long audio streaming/backpressure, guaranteed cancellation on GPU, process crash recovery and playback lifecycle remain independent tasks.
5. **ASR/VAD**: real speech capture, segmentation, device monitoring and Rust-native wavekat VAD are not implemented here.
6. **Security and durable state**: third-party plugin execution is not OS-sandboxed; durable selection/profiles revision history, trusted catalog, per-plugin permissions and UI confirmation still need implementation.

Do not mark a global issue or full Python parity complete based solely on mock tests or the number of plugin descriptors. Next objective should be an application-owned AI runtime composition/lifecycle and one instrumented real-model Silero+RVC inference using pinned assets and a verified Python 3.12 environment.
