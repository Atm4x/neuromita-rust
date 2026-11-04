//! Telegram TTS using an already authorized Telethon session and a managed Python worker.
//! Interactive authorization is intentionally outside synthesis; no credentials enter AppSettings.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

use crate::{
    adapters::pcm16::apply_volume,
    settings::Volume,
    voice::{AudioArtifact, AudioFormat, VoiceError},
};

const PYTHON_TRANSPORT: &str = include_str!("../../python/neuromita_voice/telegram_transport.py");
const MAX_AUDIO: usize = 20 * 1024 * 1024;
const MAX_WAV: usize = 100 * 1024 * 1024;

pub struct TelegramVoice {
    data_directory: PathBuf,
    serial: tokio::sync::Mutex<()>,
}

impl TelegramVoice {
    pub fn new(data_directory: PathBuf) -> Self {
        Self {
            data_directory,
            serial: tokio::sync::Mutex::new(()),
        }
    }

    pub async fn synthesize(
        &self,
        text: &str,
        speaker: &str,
        bot: &str,
        wait_seconds: u16,
        interval_seconds: u16,
        volume: Volume,
    ) -> Result<AudioArtifact, VoiceError> {
        let _guard = self.serial.lock().await;
        if !valid_bot(bot)
            || text.trim().is_empty()
            || text.len() > 8192
            || speaker.len() > 256
            || speaker.contains(['\n', '\r'])
        {
            return Err(VoiceError::Generation(
                "invalid Telegram TTS request".into(),
            ));
        }
        let python = required_executable("NEUROMITA_TELEGRAM_PYTHON")?;
        let session = if let Some(path) = std::env::var_os("NEUROMITA_TELEGRAM_SESSION") {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(VoiceError::Generation(
                    "Telegram session must use an absolute path".into(),
                ));
            }
            path
        } else {
            self.data_directory.join("AIData/telegram/speech")
        };
        let session_dir = session
            .parent()
            .ok_or_else(|| VoiceError::Generation("invalid Telegram session path".into()))?;
        tokio::fs::create_dir_all(session_dir).await.map_err(|_| {
            VoiceError::Generation("cannot prepare Telegram session directory".into())
        })?;
        if tokio::fs::symlink_metadata(session_dir)
            .await
            .is_ok_and(|info| info.file_type().is_symlink())
        {
            return Err(VoiceError::Generation(
                "Telegram session directory cannot be a symlink".into(),
            ));
        }
        if tokio::fs::symlink_metadata(session.with_extension("session"))
            .await
            .is_ok_and(|info| info.file_type().is_symlink())
        {
            return Err(VoiceError::Generation(
                "Telegram session path is a symlink".into(),
            ));
        }
        let payload = serde_json::json!({
            "text": text,
            "speaker": speaker,
            "bot": bot,
            "wait_seconds": wait_seconds,
            "interval_seconds": interval_seconds,
        });
        let mut worker = Command::new(python)
            .args(["-I", "-u", "-c", PYTHON_TRANSPORT])
            .env("NEUROMITA_TELEGRAM_SESSION", &session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| VoiceError::Generation("Telegram Python worker could not start".into()))?;
        let work = async {
            let mut stdin = worker.stdin.take().ok_or(VoiceError::Unavailable)?;
            stdin
                .write_all(payload.to_string().as_bytes())
                .await
                .map_err(|_| VoiceError::Generation("Telegram request IPC failed".into()))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| VoiceError::Generation("Telegram request IPC failed".into()))?;
            drop(stdin);
            let stdout = worker.stdout.take().ok_or(VoiceError::Unavailable)?;
            let mut encoded = Vec::new();
            let amount = stdout
                .take((MAX_AUDIO * 4 / 3 + 16) as u64)
                .read_to_end(&mut encoded)
                .await
                .map_err(|_| VoiceError::Generation("Telegram worker output failed".into()))?;
            if amount > MAX_AUDIO * 4 / 3 + 8 {
                return Err(VoiceError::Generation("Telegram audio is too large".into()));
            }
            let status = worker
                .wait()
                .await
                .map_err(|_| VoiceError::Generation("Telegram worker failed".into()))?;
            if !status.success() {
                return Err(VoiceError::Generation(
                    "Telegram session is unavailable or not authorized".into(),
                ));
            }
            let audio = STANDARD
                .decode(encoded)
                .map_err(|_| VoiceError::Generation("Telegram returned malformed audio".into()))?;
            if audio.is_empty() || audio.len() > MAX_AUDIO {
                return Err(VoiceError::Generation(
                    "Telegram audio is empty or too large".into(),
                ));
            }
            Ok(audio)
        };
        let encoded = timeout(
            Duration::from_secs(u64::from(wait_seconds) + u64::from(interval_seconds) + 45),
            work,
        )
        .await
        .map_err(|_| VoiceError::Generation("Telegram TTS timed out".into()))??;
        transcode_to_wav(&encoded, &self.data_directory, volume).await
    }
}

fn valid_bot(bot: &str) -> bool {
    let Some(name) = bot.strip_prefix('@') else {
        return false;
    };
    (5..=32).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn required_executable(name: &str) -> Result<PathBuf, VoiceError> {
    let exe = std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
        VoiceError::Generation(format!("{name} must point to installed executable"))
    })?;
    if !exe.is_absolute() || !exe.is_file() || exe.is_symlink() {
        return Err(VoiceError::Generation(format!(
            "{name} is not a regular absolute executable path"
        )));
    }
    Ok(exe)
}

async fn transcode_to_wav(
    audio: &[u8],
    data_directory: &Path,
    volume: Volume,
) -> Result<AudioArtifact, VoiceError> {
    let ffmpeg = std::env::var_os("NEUROMITA_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            data_directory.join(if cfg!(windows) {
                "AIData/bin/ffmpeg.exe"
            } else {
                "AIData/bin/ffmpeg"
            })
        });
    if !ffmpeg.is_absolute() || !ffmpeg.is_file() || ffmpeg.is_symlink() {
        return Err(VoiceError::Generation(
            "FFmpeg is required for Telegram TTS decoding".into(),
        ));
    }
    let mut ffmpeg = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-i",
            "pipe:0",
            "-vn",
            "-f",
            "wav",
            "-acodec",
            "pcm_s16le",
            "-ac",
            "1",
            "-ar",
            "44100",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| VoiceError::Generation("cannot start FFmpeg".into()))?;
    let mut stdin = ffmpeg.stdin.take().ok_or(VoiceError::Unavailable)?;
    let audio = audio.to_vec();
    let writer = tokio::spawn(async move {
        let result = stdin.write_all(&audio).await;
        drop(stdin);
        result
    });
    let stdout = ffmpeg.stdout.take().ok_or(VoiceError::Unavailable)?;
    let mut wav = Vec::new();
    let copied = timeout(
        Duration::from_secs(30),
        stdout.take((MAX_WAV + 1) as u64).read_to_end(&mut wav),
    )
    .await
    .map_err(|_| VoiceError::Generation("Telegram audio decode timed out".into()))?
    .map_err(|_| VoiceError::Generation("Telegram audio decode failed".into()))?;
    if copied > MAX_WAV {
        return Err(VoiceError::Generation(
            "decoded Telegram audio exceeds limit".into(),
        ));
    }
    writer
        .await
        .map_err(|_| VoiceError::Generation("audio decode input failed".into()))?
        .map_err(|_| VoiceError::Generation("audio decode input failed".into()))?;
    let status = ffmpeg
        .wait()
        .await
        .map_err(|_| VoiceError::Generation("audio decode failed".into()))?;
    if !status.success() || wav.len() < 44 || !wav.starts_with(b"RIFF") || &wav[8..12] != b"WAVE" {
        return Err(VoiceError::Generation(
            "Telegram audio conversion produced invalid WAV".into(),
        ));
    }
    let start = normalize_streamed_wav(&mut wav)?;
    apply_volume(&mut wav[start..], volume)?;
    Ok(AudioArtifact {
        format: AudioFormat::Wave,
        bytes: wav.into(),
    })
}

fn normalize_streamed_wav(wav: &mut [u8]) -> Result<usize, VoiceError> {
    if wav.len() < 44 || !wav.starts_with(b"RIFF") || &wav[8..12] != b"WAVE" {
        return Err(VoiceError::Generation(
            "invalid WAV data from Telegram decoder".into(),
        ));
    }
    let size = u32::try_from(wav.len() - 8)
        .map_err(|_| VoiceError::Generation("Telegram WAV is too large".into()))?;
    wav[4..8].copy_from_slice(&size.to_le_bytes());
    let mut pos = 12usize;
    let mut found = None;
    while pos + 8 <= wav.len() {
        let chunk_length = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if &wav[pos..pos + 4] == b"data" {
            let bytes = wav.len() - (pos + 8);
            let bytes = u32::try_from(bytes)
                .map_err(|_| VoiceError::Generation("Telegram WAV is too large".into()))?;
            wav[pos + 4..pos + 8].copy_from_slice(&bytes.to_le_bytes());
            if bytes > 0 {
                found = Some(pos + 8);
            }
            break;
        }
        let next = pos
            .checked_add(8)
            .and_then(|p| p.checked_add(chunk_length))
            .and_then(|p| p.checked_add(chunk_length % 2))
            .ok_or_else(|| VoiceError::Generation("invalid WAV chunks".into()))?;
        if next <= pos || next > wav.len() {
            break;
        }
        pos = next;
    }
    found.ok_or_else(|| VoiceError::Generation("Telegram WAV has no audio data".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streamed_wav_lengths_are_rewritten() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&u32::MAX.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&[1, 0, 1, 0]);
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&88200u32.to_le_bytes());
        wav.extend_from_slice(&[2, 0, 16, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&u32::MAX.to_le_bytes());
        wav.extend_from_slice(&[0, 1]);
        normalize_streamed_wav(&mut wav).unwrap();
        assert_eq!(&wav[4..8], &38u32.to_le_bytes());
        assert_eq!(&wav[40..44], &2u32.to_le_bytes());
    }

    #[test]
    fn telegram_target_is_explicit() {
        assert!(valid_bot("@silero_voice_bot"));
        assert!(!valid_bot("https://t.me/example"));
        assert!(!valid_bot("@bad name"));
    }
}
