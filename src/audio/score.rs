//! Stateful score providers and bounded framing; endpoint policy belongs to the segmenter.

use std::{fs::File, io::Read, path::Path, sync::Arc};

use ring::digest::{SHA256, digest};
use thiserror::Error;

use super::vad::{FRAME_SAMPLES, SAMPLE_RATE_HZ};

#[cfg(feature = "vad-silero")]
use std::{path::PathBuf, sync::Mutex};

#[cfg(feature = "vad-silero")]
enum RuntimeState {
    Uninitialized,
    Ready(PathBuf),
    Failed,
}

#[cfg(feature = "vad-silero")]
static ONNX_RUNTIME: Mutex<RuntimeState> = Mutex::new(RuntimeState::Uninitialized);

// Official Microsoft ONNX Runtime 1.24.2, Windows x64 CPU release.
#[cfg(feature = "vad-silero")]
const ORT_RUNTIME_BYTES: u64 = 14_148_680;
#[cfg(feature = "vad-silero")]
const ORT_RUNTIME_SHA256: &str = "114947d633e6844ce3c4b51ef6678f776628571d08a5763859c61642c8dcca9c";

pub const SILERO_MODEL_BYTES: usize = 2_327_524;
pub const SILERO_MODEL_SHA256: &str =
    "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3";

#[derive(Debug, Error)]
pub enum ScoreError {
    #[error("invalid VAD framing: {0}")]
    Framing(&'static str),
    #[error("VAD provider failed: {0}")]
    Provider(String),
    #[error("VAD provider returned an invalid probability")]
    Probability,
    #[error("VAD score stream failed; reset is required before continuing")]
    ResetRequired,
    #[error("local Silero asset must be an absolute path")]
    AssetPath,
    #[error("local Silero asset does not match the pinned size/SHA-256")]
    AssetMismatch,
    #[error("ONNX Runtime must be explicitly initialized before creating Silero")]
    RuntimeUninitialized,
    #[error("ONNX Runtime is already bound to a different library or initialization failed")]
    RuntimeConflict,
    #[error("ONNX Runtime library does not match the pinned Windows x64 CPU release")]
    RuntimeMismatch,
    #[error("cannot read local VAD asset: {0}")]
    AssetIo(#[from] std::io::Error),
}

pub trait VadScoreProvider: Send {
    fn frame_samples(&self) -> usize;
    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError>;
    fn reset(&mut self);
}

#[derive(Default)]
pub struct DirectEarshotScores(earshot::Detector);

impl VadScoreProvider for DirectEarshotScores {
    fn frame_samples(&self) -> usize {
        FRAME_SAMPLES
    }

    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
        let frame: &[i16; FRAME_SAMPLES] = frame
            .try_into()
            .map_err(|_| ScoreError::Framing("Earshot requires 256 samples"))?;
        Ok(self.0.predict_i16(frame))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct ScoredFrame {
    pub start_sample: u64,
    pub end_sample: u64,
    pub probability: f32,
}

pub struct ScoreFrames<P> {
    provider: P,
    pending: Vec<i16>,
    frame_samples: usize,
    scored_samples: u64,
    poisoned: bool,
}

impl<P: VadScoreProvider> ScoreFrames<P> {
    pub fn new(provider: P) -> Result<Self, ScoreError> {
        let frame_samples = provider.frame_samples();
        if ![256, 512].contains(&frame_samples) {
            return Err(ScoreError::Framing("unsupported provider frame size"));
        }
        Ok(Self {
            provider,
            pending: Vec::with_capacity(frame_samples),
            frame_samples,
            scored_samples: 0,
            poisoned: false,
        })
    }

    /// Accept at most one second of mono PCM16 at 16 kHz; never pad a partial frame.
    pub fn push(&mut self, samples: &[i16]) -> Result<Vec<ScoredFrame>, ScoreError> {
        if self.poisoned {
            return Err(ScoreError::ResetRequired);
        }
        if samples.len() > SAMPLE_RATE_HZ as usize {
            return Err(ScoreError::Framing("input chunk exceeds one second"));
        }
        let mut scores =
            Vec::with_capacity((self.pending.len() + samples.len()) / self.frame_samples);
        let mut remaining = samples;
        while !remaining.is_empty() {
            let take = (self.frame_samples - self.pending.len()).min(remaining.len());
            self.pending.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.pending.len() == self.frame_samples {
                self.poisoned = true;
                let probability = self.provider.score(&self.pending)?;
                if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
                    return Err(ScoreError::Probability);
                }
                let end_sample = self
                    .scored_samples
                    .checked_add(self.frame_samples as u64)
                    .ok_or(ScoreError::Framing("sample offset overflow"))?;
                scores.push(ScoredFrame {
                    start_sample: self.scored_samples,
                    end_sample,
                    probability,
                });
                self.scored_samples = end_sample;
                self.pending.clear();
                self.poisoned = false;
            }
        }
        Ok(scores)
    }

    pub fn pending_samples(&self) -> usize {
        self.pending.len()
    }

    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.scored_samples = 0;
        self.provider.reset();
        self.poisoned = false;
    }
}

pub struct VerifiedSileroAsset(Arc<[u8]>);

/// Initialize ONNX Runtime from one explicit absolute DLL path.
///
/// The process-wide environment is created once and reused; there is no library
/// discovery, version switching or implicit download.
#[cfg(feature = "vad-silero")]
pub fn init_onnx_runtime(path: &Path) -> Result<(), ScoreError> {
    if !path.is_absolute() {
        return Err(ScoreError::AssetPath);
    }
    if !path.is_file() {
        return Err(ScoreError::Provider(format!(
            "ONNX Runtime library not found: {}",
            path.display()
        )));
    }
    let path = path.canonicalize()?;
    let mut state = ONNX_RUNTIME
        .lock()
        .map_err(|_| ScoreError::RuntimeConflict)?;
    match &*state {
        RuntimeState::Ready(loaded) if loaded == &path => return Ok(()),
        RuntimeState::Ready(_) | RuntimeState::Failed => return Err(ScoreError::RuntimeConflict),
        RuntimeState::Uninitialized => {}
    }
    let mut bytes = Vec::new();
    File::open(&path)?
        .take(ORT_RUNTIME_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != ORT_RUNTIME_BYTES || sha256(&bytes) != ORT_RUNTIME_SHA256 {
        return Err(ScoreError::RuntimeMismatch);
    }
    *state = RuntimeState::Failed;
    let committed = ort::init_from(&path)
        .map_err(|error| ScoreError::Provider(error.to_string()))?
        .with_name("neuromita")
        .with_telemetry(false)
        .commit();
    if !committed {
        return Err(ScoreError::RuntimeConflict);
    }
    *state = RuntimeState::Ready(path);
    Ok(())
}

/// Wavekat Earshot backend, used only for the A/B comparison against the direct
/// detector. Its frame size is 256 samples like the production Earshot path.
#[cfg(feature = "vad-wavekat")]
pub struct WavekatEarshotScores(wavekat_vad::backends::earshot::EarshotVad);

#[cfg(feature = "vad-wavekat")]
impl WavekatEarshotScores {
    pub fn new() -> Self {
        Self(wavekat_vad::backends::earshot::EarshotVad::new())
    }
}

#[cfg(feature = "vad-wavekat")]
impl Default for WavekatEarshotScores {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "vad-wavekat")]
impl VadScoreProvider for WavekatEarshotScores {
    fn frame_samples(&self) -> usize {
        FRAME_SAMPLES
    }

    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
        use wavekat_vad::VoiceActivityDetector;

        if frame.len() != FRAME_SAMPLES {
            return Err(ScoreError::Framing("wavekat Earshot requires 256 samples"));
        }
        self.0
            .process(frame, SAMPLE_RATE_HZ)
            .map_err(|error| ScoreError::Provider(error.to_string()))
    }

    fn reset(&mut self) {
        use wavekat_vad::VoiceActivityDetector;

        self.0.reset();
    }
}

/// Optional Silero backend. Requires a verified local model and an explicitly
/// initialized ONNX Runtime library; the 512-sample window is bridged onto the
/// capture grid by `ScoreGate`.
#[cfg(feature = "vad-silero")]
pub struct SileroScores(wavekat_vad::backends::silero::SileroVad);

#[cfg(feature = "vad-silero")]
impl SileroScores {
    /// Build a detector from model bytes that passed size and SHA-256 checks.
    pub fn from_verified(asset: &VerifiedSileroAsset) -> Result<Self, ScoreError> {
        let runtime = ONNX_RUNTIME
            .lock()
            .map_err(|_| ScoreError::RuntimeConflict)?;
        match &*runtime {
            RuntimeState::Ready(_) => {}
            RuntimeState::Uninitialized => return Err(ScoreError::RuntimeUninitialized),
            RuntimeState::Failed => return Err(ScoreError::RuntimeConflict),
        }
        wavekat_vad::backends::silero::SileroVad::from_memory(asset.bytes(), SAMPLE_RATE_HZ)
            .map(Self)
            .map_err(|error| ScoreError::Provider(error.to_string()))
    }
}

#[cfg(feature = "vad-silero")]
impl VadScoreProvider for SileroScores {
    fn frame_samples(&self) -> usize {
        512
    }

    fn score(&mut self, frame: &[i16]) -> Result<f32, ScoreError> {
        use wavekat_vad::VoiceActivityDetector;

        if frame.len() != 512 {
            return Err(ScoreError::Framing("Silero requires 512 samples"));
        }
        self.0
            .process(frame, SAMPLE_RATE_HZ)
            .map_err(|error| ScoreError::Provider(error.to_string()))
    }

    fn reset(&mut self) {
        use wavekat_vad::VoiceActivityDetector;

        self.0.reset();
    }
}

impl VerifiedSileroAsset {
    pub fn open(path: &Path) -> Result<Self, ScoreError> {
        if !path.is_absolute() {
            return Err(ScoreError::AssetPath);
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(SILERO_MODEL_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != SILERO_MODEL_BYTES || sha256(&bytes) != SILERO_MODEL_SHA256 {
            return Err(ScoreError::AssetMismatch);
        }
        Ok(Self(Arc::from(bytes)))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Frames {
        count: usize,
        probability: f32,
    }

    impl VadScoreProvider for Frames {
        fn frame_samples(&self) -> usize {
            512
        }
        fn score(&mut self, samples: &[i16]) -> Result<f32, ScoreError> {
            assert_eq!(samples.len(), 512);
            assert!(samples.iter().all(|value| *value == self.count as i16));
            self.count += 1;
            Ok(self.probability)
        }
        fn reset(&mut self) {
            self.count = 0;
        }
    }

    #[test]
    fn partial_frames_wait_and_reset_never_splices_streams() {
        let mut frames = ScoreFrames::new(Frames {
            count: 0,
            probability: 0.5,
        })
        .unwrap();
        assert!(frames.push(&[0; 256]).unwrap().is_empty());
        let first = frames.push(&[0; 256]).unwrap();
        assert_eq!((first[0].start_sample, first[0].end_sample), (0, 512));
        assert!(frames.push(&[1; 7]).unwrap().is_empty());
        frames.reset();
        let next = frames.push(&[0; 512]).unwrap();
        assert_eq!(first, next);
        assert_eq!(frames.pending_samples(), 0);
    }

    #[test]
    fn bad_scores_and_unbounded_chunks_are_errors() {
        let mut frames = ScoreFrames::new(Frames {
            count: 0,
            probability: f32::NAN,
        })
        .unwrap();
        assert!(matches!(
            frames.push(&[0; 512]),
            Err(ScoreError::Probability)
        ));
        assert!(matches!(
            frames.push(&vec![0; 16_001]),
            Err(ScoreError::ResetRequired)
        ));
    }

    #[test]
    fn actual_direct_earshot_is_chunk_partition_invariant_and_resets() {
        let samples: Vec<_> = (0..2048)
            .map(|index| ((index * 97) % 16000) as i16)
            .collect();
        let mut whole = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        let expected = whole.push(&samples).unwrap();
        let mut split = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        let mut actual = Vec::new();
        for chunk in samples.chunks(77) {
            actual.extend(split.push(chunk).unwrap());
        }
        assert_eq!(actual, expected);
        split.reset();
        assert_eq!(split.push(&samples).unwrap(), expected);
    }

    struct FailsSecond {
        calls: usize,
        provider_error: bool,
    }

    impl VadScoreProvider for FailsSecond {
        fn frame_samples(&self) -> usize {
            256
        }
        fn score(&mut self, _: &[i16]) -> Result<f32, ScoreError> {
            self.calls += 1;
            if self.calls != 2 {
                return Ok(0.25);
            }
            if self.provider_error {
                Err(ScoreError::Provider("controlled failure".into()))
            } else {
                Ok(f32::NAN)
            }
        }
        fn reset(&mut self) {
            self.calls = 0;
        }
    }

    #[test]
    fn partial_chunk_failure_requires_reset_and_never_returns_partial_metrics() {
        for provider_error in [false, true] {
            let mut frames = ScoreFrames::new(FailsSecond {
                calls: 0,
                provider_error,
            })
            .unwrap();
            assert!(frames.push(&[0; 512]).is_err());
            assert_eq!(frames.provider.calls, 2);
            assert!(matches!(
                frames.push(&[0; 256]),
                Err(ScoreError::ResetRequired)
            ));
            assert!(matches!(frames.push(&[]), Err(ScoreError::ResetRequired)));
            assert_eq!(frames.provider.calls, 2);
            frames.reset();
            let recovered = frames.push(&[0; 256]).unwrap();
            assert_eq!(
                recovered,
                vec![ScoredFrame {
                    start_sample: 0,
                    end_sample: 256,
                    probability: 0.25
                }]
            );
        }
    }

    #[test]
    fn sample_offset_failure_also_requires_reset() {
        let mut frames = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        frames.scored_samples = u64::MAX;
        assert!(matches!(
            frames.push(&[0; 256]),
            Err(ScoreError::Framing(_))
        ));
        assert!(matches!(
            frames.push(&[0; 256]),
            Err(ScoreError::ResetRequired)
        ));
        frames.reset();
        assert_eq!(frames.push(&[0; 256]).unwrap()[0].start_sample, 0);
    }

    #[test]
    fn oversize_input_rejection_does_not_consume_or_poison() {
        let mut frames = ScoreFrames::new(DirectEarshotScores::default()).unwrap();
        assert!(frames.push(&vec![0; 16_001]).is_err());
        assert_eq!(frames.push(&[0; 256]).unwrap()[0].start_sample, 0);
    }

    #[test]
    fn direct_earshot_rejects_short_and_long_frames() {
        let mut provider = DirectEarshotScores::default();
        assert!(matches!(
            provider.score(&[0; 255]),
            Err(ScoreError::Framing(_))
        ));
        assert!(matches!(
            provider.score(&[0; 257]),
            Err(ScoreError::Framing(_))
        ));
        assert!(provider.score(&[0; 256]).is_ok());
    }

    #[cfg(feature = "vad-wavekat")]
    #[test]
    fn wavekat_earshot_scores_same_frames_as_direct_detector() {
        use super::WavekatEarshotScores;

        let mut direct = DirectEarshotScores::default();
        let mut wavekat = WavekatEarshotScores::new();
        assert_eq!(wavekat.frame_samples(), direct.frame_samples());
        for marker in [0i16, 120, 900, -400] {
            let frame = [marker; FRAME_SAMPLES];
            let expected = direct.score(&frame).unwrap();
            let actual = wavekat.score(&frame).unwrap();
            assert!((expected - actual).abs() < 1e-6);
            assert!(matches!(
                wavekat.score(&[0; 255]),
                Err(ScoreError::Framing(_))
            ));
        }
    }

    #[cfg(feature = "vad-silero")]
    #[test]
    fn onnx_runtime_init_requires_absolute_existing_library() {
        use super::init_onnx_runtime;

        assert!(matches!(
            init_onnx_runtime(Path::new("onnxruntime.dll")),
            Err(ScoreError::AssetPath)
        ));
        assert!(matches!(
            init_onnx_runtime(Path::new("C:/definitely/missing/onnxruntime.dll")),
            Err(ScoreError::Provider(_))
        ));
        let wrong = std::env::temp_dir().join(format!("vad-not-a-dll-{}", uuid::Uuid::new_v4()));
        std::fs::write(&wrong, b"not an ONNX Runtime binary").unwrap();
        let result = init_onnx_runtime(&wrong);
        std::fs::remove_file(wrong).unwrap();
        assert!(matches!(result, Err(ScoreError::RuntimeMismatch)));
    }

    #[cfg(feature = "vad-silero")]
    #[test]
    fn silero_constructor_requires_our_runtime_initialization() {
        let asset = VerifiedSileroAsset(Arc::from([]));
        assert!(matches!(
            SileroScores::from_verified(&asset),
            Err(ScoreError::RuntimeUninitialized)
        ));
    }

    /// Live check with the pinned model and an explicit ONNX Runtime library.
    /// Ignored by default: set both variables to run it.
    #[cfg(feature = "vad-silero")]
    #[test]
    #[ignore = "requires NEUROMITA_SILERO_MODEL and NEUROMITA_ORT_DLL"]
    fn live_silero_scores_silence_and_tone() {
        use super::{SileroScores, init_onnx_runtime};

        let model = std::env::var("NEUROMITA_SILERO_MODEL").expect("model path");
        let library = std::env::var("NEUROMITA_ORT_DLL").expect("onnxruntime library path");
        init_onnx_runtime(Path::new(&library)).unwrap();
        init_onnx_runtime(Path::new(&library)).unwrap();
        assert!(matches!(
            init_onnx_runtime(Path::new(&model)),
            Err(ScoreError::RuntimeConflict)
        ));
        let asset = VerifiedSileroAsset::open(Path::new(&model)).unwrap();
        let mut frames = ScoreFrames::new(SileroScores::from_verified(&asset).unwrap()).unwrap();

        let silence: Vec<i16> = vec![0; SAMPLE_RATE_HZ as usize];
        let quiet = frames.push(&silence).unwrap();
        let tone: Vec<i16> = (0..SAMPLE_RATE_HZ as usize / 2)
            .map(|index| {
                let t = index as f64 / SAMPLE_RATE_HZ as f64;
                ((2.0 * std::f64::consts::PI * 180.0 * t).sin() * 6000.0) as i16
            })
            .collect();
        let voiced = frames.push(&tone).unwrap();

        let quiet_max = quiet.iter().map(|s| s.probability).fold(0.0, f32::max);
        let voiced_max = voiced.iter().map(|s| s.probability).fold(0.0, f32::max);
        assert!(quiet_max < 0.2, "silence scored {quiet_max}");
        assert!(voiced_max > quiet_max, "tone did not outscore silence");
        frames.reset();
        let reset_quiet = frames.push(&silence).unwrap();
        let reset_voiced = frames.push(&tone).unwrap();
        assert_eq!(
            quiet, reset_quiet,
            "reset must clear recurrent state, context and offsets"
        );
        assert_eq!(voiced, reset_voiced);
    }
}
