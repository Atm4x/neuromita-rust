//! Application-owned microphone session coordination. The audio and ASR pipeline
//! remains in `SpeechInputHandler`; this component owns its task and controls.

use std::{path::PathBuf, sync::Arc};

use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::settings::AsrPreferences;

use super::{
    device::{CaptureBackend, FfmpegCapture, FfplayOutput},
    segment::{SegmentConfig, SpeechMode, SpeechSegmenter},
    session::{SpeechControl, SpeechInputHandler, SpeechSessionError},
    vad::{EarshotVad, VoiceThreshold},
};

const FRAME_MS: usize = 16;
const MAX_SPEECH_FRAMES: usize = 625;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechSessionPhase {
    Stopped,
    Starting,
    Listening,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSessionSnapshot {
    pub phase: SpeechSessionPhase,
    pub last_error: Option<String>,
}

impl Default for SpeechSessionSnapshot {
    fn default() -> Self {
        Self {
            phase: SpeechSessionPhase::Stopped,
            last_error: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum SpeechCaptureError {
    #[error("speech session is already running")]
    AlreadyRunning,
    #[error("speech session is not running")]
    NotRunning,
    #[error("invalid speech capture configuration: {0}")]
    Invalid(String),
    #[error(transparent)]
    Session(#[from] SpeechSessionError),
    #[error("speech session task failed: {0}")]
    Task(String),
}

pub struct SpeechCaptureConfig {
    pub ffmpeg: PathBuf,
    pub ffplay: PathBuf,
    pub backend: CaptureBackend,
    pub device: String,
    pub threshold: VoiceThreshold,
    pub segment: SegmentConfig,
    pub radio_mode: bool,
}

impl SpeechCaptureConfig {
    pub fn from_preferences(
        preferences: &AsrPreferences,
        ffmpeg: PathBuf,
        ffplay: PathBuf,
    ) -> Result<Self, SpeechCaptureError> {
        let backend = match preferences.audio_backend.as_str() {
            "pulse" => CaptureBackend::Pulse,
            "alsa" => CaptureBackend::Alsa,
            "dshow" => CaptureBackend::Dshow,
            "avfoundation" => CaptureBackend::Avfoundation,
            other => {
                return Err(SpeechCaptureError::Invalid(format!(
                    "unsupported backend: {other}"
                )));
            }
        };
        let mode = match preferences.input_mode.as_str() {
            "continuous" => SpeechMode::Automatic,
            "radio" | "push_to_talk" => SpeechMode::PushToTalk,
            other => {
                return Err(SpeechCaptureError::Invalid(format!(
                    "unsupported input mode: {other}"
                )));
            }
        };
        if !ffmpeg.is_absolute() || !ffplay.is_absolute() || !ffmpeg.is_file() || !ffplay.is_file()
        {
            return Err(SpeechCaptureError::Invalid(
                "explicit FFmpeg and FFplay binaries are required".into(),
            ));
        }
        if preferences.device_id.is_empty()
            || preferences.device_id.len() > 512
            || preferences.device_id.contains('\0')
        {
            return Err(SpeechCaptureError::Invalid(
                "explicit audio input identity required".into(),
            ));
        }
        let threshold = VoiceThreshold::new(f32::from(preferences.vad_threshold_milli) / 1000.0)
            .map_err(|error| SpeechCaptureError::Invalid(error.to_string()))?;
        let frames = |ms: u32| -> usize { (ms as usize).div_ceil(FRAME_MS) };
        // The IPC request and WAV bounds currently limit an individual phrase to 10 seconds.
        let max_frames = frames(preferences.max_speech_ms).min(MAX_SPEECH_FRAMES);
        let min_frames = frames(preferences.min_speech_ms).max(1);
        let segment = SegmentConfig {
            mode,
            pre_roll_frames: frames(preferences.pre_buffer_ms),
            start_voice_frames: 3,
            end_silence_frames: frames(preferences.silence_ms).max(1),
            min_segment_frames: min_frames,
            max_segment_frames: max_frames,
        };
        if segment.min_segment_frames > segment.max_segment_frames
            || segment.pre_roll_frames > segment.max_segment_frames
            || preferences.chunk_size == 0
        {
            return Err(SpeechCaptureError::Invalid(
                "incompatible segmentation limits".into(),
            ));
        }
        Ok(Self {
            ffmpeg,
            ffplay,
            backend,
            device: preferences.device_id.clone(),
            threshold,
            segment,
            radio_mode: preferences.input_mode == "radio",
        })
    }
}

struct ActiveSession {
    stop: CancellationToken,
    control: mpsc::Sender<SpeechControl>,
    task: JoinHandle<()>,
}

pub struct SpeechSessionController {
    sender: watch::Sender<SpeechSessionSnapshot>,
    active: Option<ActiveSession>,
}

impl Default for SpeechSessionController {
    fn default() -> Self {
        let (sender, _) = watch::channel(SpeechSessionSnapshot::default());
        Self {
            sender,
            active: None,
        }
    }
}

impl SpeechSessionController {
    pub fn observe(&self) -> watch::Receiver<SpeechSessionSnapshot> {
        self.sender.subscribe()
    }

    pub fn snapshot(&self) -> SpeechSessionSnapshot {
        self.sender.borrow().clone()
    }

    pub async fn start(
        &mut self,
        handler: SpeechInputHandler,
        stop: CancellationToken,
        config: SpeechCaptureConfig,
    ) -> Result<(), SpeechCaptureError> {
        self.reap().await;
        if self.active.is_some() {
            return Err(SpeechCaptureError::AlreadyRunning);
        }
        if stop.is_cancelled() {
            return Err(SpeechCaptureError::Invalid(
                "application is shutting down".into(),
            ));
        }
        self.sender.send_replace(SpeechSessionSnapshot {
            phase: SpeechSessionPhase::Starting,
            last_error: None,
        });
        let setup = (|| {
            let capture = FfmpegCapture::start(config.ffmpeg, config.backend, &config.device)
                .map_err(SpeechSessionError::from)?;
            let segmenter =
                SpeechSegmenter::new(EarshotVad::new(config.threshold), config.segment)?;
            Ok::<_, SpeechSessionError>((capture, segmenter))
        })();
        let (capture, segmenter) = match setup {
            Ok(value) => value,
            Err(error) => {
                self.sender.send_replace(SpeechSessionSnapshot {
                    phase: SpeechSessionPhase::Failed,
                    last_error: Some(error.to_string()),
                });
                return Err(error.into());
            }
        };
        let (control, receive) = mpsc::channel(8);
        if config.radio_mode {
            control
                .try_send(SpeechControl::PushToTalk(true))
                .map_err(|error| SpeechCaptureError::Invalid(error.to_string()))?;
        }
        let sender = self.sender.clone();
        let run_stop = stop.clone();
        let task = tokio::spawn(async move {
            let result = handler
                .run(capture, segmenter, receive, run_stop.clone())
                .await;
            sender.send_replace(match result {
                Ok(()) => SpeechSessionSnapshot::default(),
                Err(_error) if run_stop.is_cancelled() => SpeechSessionSnapshot::default(),
                Err(error) => SpeechSessionSnapshot {
                    phase: SpeechSessionPhase::Failed,
                    last_error: Some(error.to_string()),
                },
            });
        });
        self.active = Some(ActiveSession {
            stop,
            control,
            task,
        });
        self.sender.send_replace(SpeechSessionSnapshot {
            phase: SpeechSessionPhase::Listening,
            last_error: None,
        });
        Ok(())
    }

    pub async fn press(&mut self, pressed: bool) -> Result<(), SpeechCaptureError> {
        self.reap().await;
        let active = self.active.as_ref().ok_or(SpeechCaptureError::NotRunning)?;
        active
            .control
            .send(SpeechControl::PushToTalk(pressed))
            .await
            .map_err(|_| SpeechCaptureError::NotRunning)
    }

    pub async fn stop(&mut self) {
        if let Some(active) = self.active.take() {
            self.sender.send_replace(SpeechSessionSnapshot {
                phase: SpeechSessionPhase::Stopping,
                last_error: None,
            });
            active.stop.cancel();
            let _ = active.task.await;
            self.sender.send_replace(SpeechSessionSnapshot::default());
        }
    }

    async fn reap(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.task.is_finished())
            && let Some(active) = self.active.take()
        {
            let _ = active.task.await;
        }
    }
}

impl Drop for SpeechSessionController {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop.cancel();
            active.task.abort();
        }
    }
}

pub fn playback(config: &SpeechCaptureConfig) -> Arc<dyn super::device::AudioPlayback> {
    Arc::new(FfplayOutput {
        binary: config.ffplay.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_begins_stopped_and_rejects_ptt_without_capture() {
        let mut owner = SpeechSessionController::default();
        assert_eq!(owner.snapshot().phase, SpeechSessionPhase::Stopped);
        assert!(matches!(
            owner.press(true).await,
            Err(SpeechCaptureError::NotRunning)
        ));
        owner.stop().await;
        assert_eq!(owner.snapshot().phase, SpeechSessionPhase::Stopped);
    }

    #[test]
    fn capture_profile_maps_saved_modes_and_caps_ipc_segments() {
        let binary = std::env::current_exe().unwrap();
        let mut settings = AsrPreferences {
            device_id: "mic0".into(),
            audio_backend: "pulse".into(),
            input_mode: "continuous".into(),
            ..Default::default()
        };
        let automatic =
            SpeechCaptureConfig::from_preferences(&settings, binary.clone(), binary.clone())
                .unwrap();
        assert_eq!(automatic.segment.mode, SpeechMode::Automatic);
        assert_eq!(automatic.segment.max_segment_frames, MAX_SPEECH_FRAMES);
        assert_eq!(automatic.segment.pre_roll_frames, 25);
        settings.input_mode = "radio".into();
        let radio =
            SpeechCaptureConfig::from_preferences(&settings, binary.clone(), binary.clone())
                .unwrap();
        assert!(radio.radio_mode);
        assert_eq!(radio.segment.mode, SpeechMode::PushToTalk);
        settings.input_mode = "push_to_talk".into();
        let held = SpeechCaptureConfig::from_preferences(&settings, binary.clone(), binary.clone())
            .unwrap();
        assert!(!held.radio_mode);
        assert_eq!(held.segment.mode, SpeechMode::PushToTalk);
        settings.audio_backend = "unsupported".into();
        assert!(SpeechCaptureConfig::from_preferences(&settings, binary.clone(), binary).is_err());
    }
    #[test]
    fn device_identity_is_not_guessed() {
        let mut settings = AsrPreferences::default();
        settings.device_id = "".into();
        let result =
            SpeechCaptureConfig::from_preferences(&settings, PathBuf::new(), PathBuf::new());
        assert!(matches!(result, Err(SpeechCaptureError::Invalid(_))));
    }
}
