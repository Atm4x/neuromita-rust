//! Self-contained Beat Sync execution and Python-compatible game event payloads.
//! A caller supplies an immutable source snapshot; real model worker adapters
//! can be added without bypassing the authoritative cache.
use super::{
    BeatAnalysisOutcome, BeatAnalysisRequest, BeatBackend, BeatCache, BeatCacheError,
    BeatPipelineError, dsp::detect_beats, wav::{WavError, decode_wav},
};
use serde_json::{Value, json};
use std::{fs, io, path::Path};
use thiserror::Error;

pub const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const DSP_REVISION: &str = "dsp-energy-onset-v1";
const DECODER_REVISION: &str = "wav-pcm-v1";
const SETTINGS_REVISION: &str = "energy-window10ms-v1";

#[derive(Debug, Error)]
pub enum BeatRuntimeError {
    #[error("Beat Sync is disabled")]
    Disabled,
    #[error("source media is empty or exceeds 64 MiB")]
    SourceSize,
    #[error("unsupported beat backend; model execution is not configured")]
    BackendUnavailable,
    #[error("audio path is required")]
    MissingPath,
    #[error("audio source could not be read: {0}")]
    Io(#[from] io::Error),
    #[error("WAV decoding or beat analysis failed: {0}")]
    Audio(#[from] WavError),
    #[error("beat cache/identity failed: {0}")]
    Cache(#[from] BeatCacheError),
}

#[derive(Debug, Clone)]
pub struct MusicJob<'a> {
    pub request_id: &'a str,
    pub track_name: &'a str,
    pub audio_path: &'a str,
    pub stream: bool,
    pub min_confidence: f32,
    pub backend: BeatBackend,
    pub enabled: bool,
}

/// Owner of a native DSP implementation. BeatThis/Librosa are deliberately
/// unavailable until a verified model adapter and readiness implementation exist.
pub struct BeatRuntime {
    pub cache: BeatCache,
}

impl BeatRuntime {
    pub fn new(cache: BeatCache) -> Self {
        Self { cache }
    }

    /// Loads one bounded input snapshot; processing never re-opens its path.
    pub fn read_source(path: &Path) -> Result<Vec<u8>, BeatRuntimeError> {
        if path.as_os_str().is_empty() {
            return Err(BeatRuntimeError::MissingPath);
        }
        let meta = fs::metadata(path)?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SOURCE_BYTES {
            return Err(BeatRuntimeError::SourceSize);
        }
        let mut file = fs::File::open(path)?;
        use io::Read;
        let mut bytes = Vec::new();
        file.by_ref().take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_SOURCE_BYTES {
            return Err(BeatRuntimeError::SourceSize);
        }
        Ok(bytes)
    }

    /// The native DSP path is fully usable even without Python/model packages.
    /// Auto explicitly chooses native DSP as the last available backend.
    pub fn analyze(
        &self,
        audio: &[u8],
        job: &MusicJob<'_>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<BeatAnalysisOutcome, BeatRuntimeError> {
        if !job.enabled {
            return Err(BeatRuntimeError::Disabled);
        }
        if audio.is_empty() || audio.len() as u64 > MAX_SOURCE_BYTES {
            return Err(BeatRuntimeError::SourceSize);
        }
        if cancelled() {
            return Err(BeatRuntimeError::Audio(WavError::Cancelled));
        }
        if !matches!(job.backend, BeatBackend::Auto | BeatBackend::Dsp) {
            return Err(BeatRuntimeError::BackendUnavailable);
        }
        let outcome = self.cache.analyze_or_reuse(
            audio,
            BeatAnalysisRequest {
                backend: job.backend,
                backend_revision: DSP_REVISION,
                decoder_revision: DECODER_REVISION,
                analysis_settings_revision: SETTINGS_REVISION,
                min_confidence: job.min_confidence,
            },
            |source| {
                let wav = decode_wav(source, cancelled)?;
                detect_beats(&wav, cancelled)
            },
        );
        let outcome = match outcome {
            Ok(value) => value,
            Err(BeatPipelineError::Cache(error)) => return Err(error.into()),
            Err(BeatPipelineError::Analyzer(error)) => return Err(error.into()),
        };
        if cancelled() {
            return Err(BeatRuntimeError::Audio(WavError::Cancelled));
        }
        Ok(outcome)
    }

    /// Emits Python-compatible started/chunk/ready/error frames. Like the
    /// current Python implementation, streaming is a single *completed*
    /// chunk, not progressive inference. Caller transports these frames.
    pub fn run(
        &self,
        audio: &[u8],
        job: &MusicJob<'_>,
        cancelled: &impl Fn() -> bool,
        mut emit: impl FnMut(Value),
    ) -> Result<(), BeatRuntimeError> {
        let identity = json!({
            "track_name": job.track_name,
            "audio_path": job.audio_path,
            "request_id": job.request_id,
        });
        let emit_error = |error: &BeatRuntimeError, emit: &mut dyn FnMut(Value)| {
            let mut body = identity.clone();
            body["error"] = json!(error.to_string());
            emit(json!({"type":"music_beats_error","body":body}));
        };
        if !job.enabled || job.audio_path.is_empty() {
            let error = if !job.enabled {
                BeatRuntimeError::Disabled
            } else {
                BeatRuntimeError::MissingPath
            };
            emit_error(&error, &mut emit);
            return Err(error);
        }
        if cancelled() {
            let error = BeatRuntimeError::Audio(WavError::Cancelled);
            emit_error(&error, &mut emit);
            return Err(error);
        }
        let mut started = identity.clone();
        started["stream"] = json!(job.stream);
        emit(json!({"type":"music_beats_job_started","body":started}));
        let outcome = match self.analyze(audio, job, cancelled) {
            Ok(value) => value,
            Err(error) => {
                emit_error(&error, &mut emit);
                return Err(error);
            }
        };
        let timeline = outcome.timeline;
        let beats: Vec<Value> = timeline.beats.iter().map(|point| {
            json!({"time":point.time_seconds,"confidence":point.confidence})
        }).collect();
        if job.stream {
            let mut chunk = identity.clone();
            chunk["chunk_index"] = json!(0);
            chunk["chunks_total"] = json!(1);
            chunk["progress"] = json!(1.0);
            chunk["beats"] = json!(beats);
            chunk["method"] = json!(timeline.method);
            emit(json!({"type":"music_beats_chunk","body":chunk}));
        }
        let mut ready = identity;
        ready["duration"] = json!(timeline.duration_seconds);
        ready["beats_count"] = json!(beats.len());
        ready["method"] = json!(timeline.method);
        ready["bpm_estimate"] = json!(timeline.bpm.unwrap_or(0.0));
        if !job.stream {
            ready["beats"] = json!(beats);
        }
        emit(json!({"type":"music_beats_ready","body":ready}));
        Ok(())
    }
}
