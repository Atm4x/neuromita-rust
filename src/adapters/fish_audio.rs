//! Fish Audio transport. The credential is intentionally kept outside AppSettings.

use std::{path::PathBuf, sync::Arc, time::Duration};

use reqwest::{
    Client,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
};
use serde_json::json;

use crate::{
    adapters::pcm16::apply_volume,
    settings::Volume,
    voice::{AudioArtifact, AudioFormat, VoiceError},
};

const ENDPOINT: &str = "https://api.fish.audio/v1/tts";
const SAMPLE_RATE: u32 = 44_100;
const MAX_PCM_BYTES: usize = 100 * 1024 * 1024;

pub struct FishAudioVoice {
    client: Client,
    credential_path: PathBuf,
}

impl FishAudioVoice {
    pub fn new(credential_path: PathBuf) -> Result<Self, VoiceError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                VoiceError::Generation("cannot initialize Fish Audio HTTP client".into())
            })?;
        Ok(Self {
            client,
            credential_path,
        })
    }

    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
        provider: &str,
        model: &str,
        speed_percent: u16,
        volume: Volume,
    ) -> Result<AudioArtifact, VoiceError> {
        let request = FishAudioRequest::new(text, voice, provider, model, speed_percent)?;
        let key = self.read_key().await?;
        let header = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| VoiceError::Generation("invalid Fish Audio credential".into()))?;
        let mut response = self
            .client
            .post(ENDPOINT)
            .header(AUTHORIZATION, header)
            .header("model", request.model)
            .json(&request.body)
            .send()
            .await
            .map_err(|_| VoiceError::Generation("Fish Audio request failed".into()))?;
        if !response.status().is_success() {
            return Err(VoiceError::Generation(
                http_reason(response.status().as_u16()).into(),
            ));
        }
        if response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.to_ascii_lowercase().contains("json") || value.starts_with("text/")
            })
        {
            return Err(VoiceError::Generation(
                "Fish Audio returned text instead of PCM".into(),
            ));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_PCM_BYTES as u64)
        {
            return Err(VoiceError::Generation(
                "Fish Audio response exceeds audio limit".into(),
            ));
        }
        let mut pcm = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| VoiceError::Generation("Fish Audio audio stream failed".into()))?
        {
            if chunk.len() > MAX_PCM_BYTES - pcm.len() {
                return Err(VoiceError::Generation(
                    "Fish Audio response exceeds audio limit".into(),
                ));
            }
            pcm.extend_from_slice(&chunk);
        }
        apply_volume(&mut pcm, volume)?;
        let wav = pcm_to_wav(&pcm)?;
        Ok(AudioArtifact {
            format: AudioFormat::Wave,
            bytes: Arc::from(wav),
        })
    }

    async fn read_key(&self) -> Result<String, VoiceError> {
        if let Some(key) = std::env::var_os("NEUROMITA_FISH_AUDIO_API_KEY") {
            return validate_key(&key.to_string_lossy());
        }
        let raw = tokio::fs::read_to_string(&self.credential_path)
            .await
            .map_err(|_| {
                VoiceError::Generation("Fish Audio credential is not configured".into())
            })?;
        validate_key(&raw)
    }

    pub fn credential_file(&self) -> &std::path::Path {
        &self.credential_path
    }
}

pub fn validate_key(value: &str) -> Result<String, VoiceError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 4096
        || !trimmed.bytes().all(|byte| (33..=126).contains(&byte))
    {
        return Err(VoiceError::Generation(
            "Fish Audio credential is missing or invalid".into(),
        ));
    }
    Ok(trimmed.into())
}

pub async fn save_key(path: &std::path::Path, key: &str) -> Result<(), VoiceError> {
    let key = validate_key(key)?;
    let dir = path
        .parent()
        .ok_or_else(|| VoiceError::Generation("invalid credential path".into()))?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|_| VoiceError::Generation("cannot prepare credential directory".into()))?;
    let metadata = tokio::fs::symlink_metadata(path).await.ok();
    if metadata.is_some_and(|meta| meta.file_type().is_symlink() || !meta.is_file()) {
        return Err(VoiceError::Generation(
            "credential path must be a regular file".into(),
        ));
    }
    let name = format!(".fish-audio-{}.tmp", uuid::Uuid::new_v4());
    let temp = dir.join(name);
    let result = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp)
            .await
            .map_err(|_| VoiceError::Generation("cannot create credential file".into()))?;
        use tokio::io::AsyncWriteExt;
        file.write_all(key.as_bytes())
            .await
            .map_err(|_| VoiceError::Generation("cannot write credential file".into()))?;
        file.sync_all()
            .await
            .map_err(|_| VoiceError::Generation("cannot sync credential file".into()))?;
        drop(file);
        tokio::fs::rename(&temp, path)
            .await
            .map_err(|_| VoiceError::Generation("cannot replace credential file".into()))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

struct FishAudioRequest {
    model: String,
    body: serde_json::Value,
}

impl FishAudioRequest {
    fn new(
        text: &str,
        voice: &str,
        provider: &str,
        model: &str,
        speed_percent: u16,
    ) -> Result<Self, VoiceError> {
        if provider != "fish_audio" {
            return Err(VoiceError::Generation(
                "unsupported remote TTS provider".into(),
            ));
        }
        if !["s2.1-pro-free", "s1", "s2-pro", "s2.1-pro"].contains(&model) {
            return Err(VoiceError::Generation(
                "unsupported Fish Audio model".into(),
            ));
        }
        let voice = normalize_voice(voice)?;
        if text.trim().is_empty() || text.len() > 32_768 {
            return Err(VoiceError::Generation(
                "text is empty or too long for remote voice".into(),
            ));
        }
        if !(50..=200).contains(&speed_percent) {
            return Err(VoiceError::Generation("invalid remote TTS speed".into()));
        }
        let body = json!({
            "text": text,
            "reference_id": voice,
            "format": "pcm",
            "sample_rate": SAMPLE_RATE,
            "prosody": { "speed": f64::from(speed_percent) / 100.0, "normalize_loudness": true }
        });
        Ok(Self {
            model: model.to_owned(),
            body,
        })
    }
}

fn normalize_voice(value: &str) -> Result<String, VoiceError> {
    let value = value.trim();
    let id = if let Some(path) = value
        .strip_prefix("https://fish.audio/")
        .or_else(|| value.strip_prefix("https://www.fish.audio/"))
    {
        path.split('/')
            .find(|segment| {
                segment.len() == 32 && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .ok_or_else(|| {
                VoiceError::Generation("Fish Audio link lacks a voice identifier".into())
            })?
    } else {
        value
    };
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(VoiceError::Generation(
            "Fish Audio voice must be a 32-character hexadecimal ID".into(),
        ));
    }
    Ok(id.to_ascii_lowercase())
}

fn pcm_to_wav(pcm: &[u8]) -> Result<Vec<u8>, VoiceError> {
    if pcm.is_empty()
        || pcm.len() % 2 != 0
        || pcm.len() > MAX_PCM_BYTES
        || pcm.len() > u32::MAX as usize - 36
    {
        return Err(VoiceError::Generation(
            "Fish Audio returned empty or malformed PCM".into(),
        ));
    }
    let mut wav = Vec::with_capacity(pcm.len() + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(pcm.len() as u32 + 36).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(wav)
}

fn http_reason(code: u16) -> &'static str {
    match code {
        401 => "Fish Audio: API key was rejected",
        402 => "Fish Audio: insufficient account balance",
        403 => "Fish Audio: access denied",
        404 => "Fish Audio: voice not found",
        422 => "Fish Audio: voice/model combination is invalid",
        429 => "Fish Audio: rate limit reached",
        _ => "Fish Audio: HTTP request rejected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fish_request_is_python_compatible() {
        let request = FishAudioRequest::new(
            "hello",
            "0123456789abcdef0123456789ABCDEF",
            "fish_audio",
            "s2.1-pro-free",
            100,
        )
        .unwrap();
        assert_eq!(request.model, "s2.1-pro-free");
        assert_eq!(request.body["sample_rate"], 44_100);
        assert_eq!(request.body["format"], "pcm");
        assert_eq!(
            request.body["reference_id"],
            "0123456789abcdef0123456789abcdef"
        );
        assert_eq!(request.body["prosody"]["speed"], 1.0);
    }

    #[tokio::test]
    async fn saved_api_key_stays_outside_settings_and_is_private() {
        let root = std::env::temp_dir().join(format!("neuromita-fish-{}", uuid::Uuid::new_v4()));
        let path = root.join("secrets/key");
        save_key(&path, "test-secret-token").await.unwrap();
        let saved = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(saved, "test-secret-token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert!(validate_key("token\nInjected-Header: yes").is_err());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn malformed_voice_and_pcm_are_rejected() {
        assert!(
            FishAudioRequest::new("hello", "invalid", "fish_audio", "s2.1-pro-free", 100).is_err()
        );
        assert!(pcm_to_wav(&[]).is_err());
        assert!(pcm_to_wav(&[0]).is_err());
        let wav = pcm_to_wav(&[0, 1, 2, 3]).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(&wav[40..44], &4_u32.to_le_bytes());
    }
}
