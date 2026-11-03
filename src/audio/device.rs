//! Explicit FFmpeg device adapters. The binaries are supplied by the host application;
//! neither shell commands nor system-default device names are guessed by the core.

use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdout, Command},
};
use tokio_util::sync::CancellationToken;

use crate::voice::{AudioArtifact, AudioFormat};

use super::vad::{FRAME_SAMPLES, MonoPcm16Frame};

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("invalid audio device configuration: {0}")]
    Invalid(String),
    #[error("audio device I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("audio device exited unexpectedly: {0}")]
    Stopped(String),
    #[error("unsupported audio format")]
    UnsupportedFormat,
}

pub type DeviceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, DeviceError>> + Send + 'a>>;

pub trait AudioCapture: Send {
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>>;
}

pub trait AudioPlayback: Send + Sync {
    fn play<'a>(&'a self, audio: AudioArtifact, stop: CancellationToken) -> DeviceFuture<'a, ()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    Pulse,
    Alsa,
    Dshow,
    Avfoundation,
    Lavfi,
}

impl CaptureBackend {
    fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Pulse => "pulse",
            Self::Alsa => "alsa",
            Self::Dshow => "dshow",
            Self::Avfoundation => "avfoundation",
            Self::Lavfi => "lavfi",
        }
    }
}

pub struct FfmpegCapture {
    child: Child,
    stdout: ChildStdout,
}

impl FfmpegCapture {
    pub fn start(
        binary: PathBuf,
        backend: CaptureBackend,
        device: &str,
    ) -> Result<Self, DeviceError> {
        if !binary.is_absolute() || device.is_empty() || device.len() > 512 || device.contains('\0')
        {
            return Err(DeviceError::Invalid(
                "explicit binary and device identity required".into(),
            ));
        }
        let mut child = Command::new(binary)
            .args([
                "-hide_banner",
                "-nostats",
                "-loglevel",
                "error",
                "-f",
                backend.ffmpeg_name(),
                "-i",
                device,
                "-ac",
                "1",
                "-ar",
                "16000",
                "-f",
                "s16le",
                "pipe:1",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DeviceError::Invalid("no capture pipe".into()))?;
        Ok(Self { child, stdout })
    }
}

impl AudioCapture for FfmpegCapture {
    fn next_frame(&mut self) -> DeviceFuture<'_, Option<MonoPcm16Frame>> {
        Box::pin(async move {
            let mut bytes = [0u8; FRAME_SAMPLES * 2];
            match self.stdout.read_exact(&mut bytes).await {
                Ok(_) => {
                    let frame = std::array::from_fn(|i| {
                        i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]])
                    });
                    Ok(Some(frame))
                }
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    let status = self.child.try_wait()?;
                    match status {
                        Some(exit) if exit.success() => Ok(None),
                        _ => Err(DeviceError::Stopped(format!("capture ended: {status:?}"))),
                    }
                }
                Err(error) => Err(error.into()),
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct FfplayOutput {
    pub binary: PathBuf,
}

impl AudioPlayback for FfplayOutput {
    fn play<'a>(&'a self, audio: AudioArtifact, stop: CancellationToken) -> DeviceFuture<'a, ()> {
        Box::pin(async move {
            if !self.binary.is_absolute() {
                return Err(DeviceError::Invalid(
                    "FFplay binary must be absolute".into(),
                ));
            }
            if audio.format != AudioFormat::Wave
                || audio.bytes.len() < 44
                || audio.bytes.len() > 64 * 1024 * 1024
                || &audio.bytes[..4] != b"RIFF"
                || &audio.bytes[8..12] != b"WAVE"
            {
                return Err(DeviceError::UnsupportedFormat);
            }
            let mut child = Command::new(&self.binary)
                .args([
                    "-hide_banner",
                    "-nodisp",
                    "-autoexit",
                    "-loglevel",
                    "error",
                    "-i",
                    "pipe:0",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| DeviceError::Invalid("no playback pipe".into()))?;
            tokio::select! {
                biased;
                _ = stop.cancelled() => { let _ = child.kill().await; return Ok(()); }
                write = stdin.write_all(&audio.bytes) => write?,
            }
            drop(stdin);
            let status = tokio::select! {
                biased;
                _ = stop.cancelled() => { let _ = child.kill().await; return Ok(()); }
                result = child.wait() => result?,
            };
            if status.success() {
                Ok(())
            } else {
                Err(DeviceError::Stopped(format!("playback exited {status}")))
            }
        })
    }
}

pub struct SilentPlayback;
impl AudioPlayback for SilentPlayback {
    fn play<'a>(&'a self, _audio: AudioArtifact, _stop: CancellationToken) -> DeviceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub type SharedPlayback = Arc<dyn AudioPlayback>;
