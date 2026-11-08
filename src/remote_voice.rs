//! Fish Audio cloud voice presets. Credentials are stored separately from exported AppSettings.
//! The storage file is access-restricted on Unix but is not encrypted at rest.
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    character::CharacterId,
    voice::{AudioArtifact, AudioFormat},
};

const ENDPOINT: &str = "https://api.fish.audio/v1/tts";
const MAX_PCM: usize = 64 * 1024 * 1024 - 44;
const MODEL_IDS: &[&str] = &["s2.1-pro-free", "s1", "s2-pro", "s2.1-pro"];

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteVoicePreset {
    pub id: String,
    pub name: String,
    pub template_id: String,
    pub api_key: String,
    pub model: String,
    pub speed_percent: u16,
    pub default_voice_id: String,
    pub character_voice_ids: BTreeMap<CharacterId, String>,
}

impl RemoteVoicePreset {
    fn fresh() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Fish Audio".into(),
            template_id: "fish_audio".into(),
            api_key: String::new(),
            model: "s2.1-pro-free".into(),
            speed_percent: 100,
            default_voice_id: String::new(),
            character_voice_ids: BTreeMap::new(),
        }
    }

    pub fn voice_for(&self, character: Option<CharacterId>) -> Result<String, RemoteVoiceError> {
        // A nonempty but invalid per-character value must never silently use someone else's voice.
        let voice = character
            .and_then(|id| self.character_voice_ids.get(&id))
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.default_voice_id);
        normalize_voice_id(voice)
    }

    pub fn validate(&self) -> Result<(), RemoteVoiceError> {
        if uuid::Uuid::parse_str(&self.id).is_err() && self.id != "fish_audio_default" {
            return Err(RemoteVoiceError::InvalidPreset("invalid preset ID".into()));
        }
        if self.template_id != "fish_audio"
            || self.name.trim().is_empty()
            || self.name.len() > 128
            || !MODEL_IDS.contains(&self.model.as_str())
            || !(50..=200).contains(&self.speed_percent)
            || self.api_key.len() > 1024
            || !self.api_key.bytes().all(|b| b.is_ascii_graphic())
            || self.character_voice_ids.len() > CharacterId::ALL.len()
        {
            return Err(RemoteVoiceError::InvalidPreset(
                "invalid Fish Audio preset settings".into(),
            ));
        }
        for id in std::iter::once(&self.default_voice_id).chain(self.character_voice_ids.values()) {
            if !id.is_empty() {
                normalize_voice_id(id)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteVoiceConfig {
    pub version: u8,
    pub active_id: String,
    pub presets: Vec<RemoteVoicePreset>,
}
impl Default for RemoteVoiceConfig {
    fn default() -> Self {
        let mut preset = RemoteVoicePreset::fresh();
        preset.id = "fish_audio_default".into();
        Self {
            version: 1,
            active_id: preset.id.clone(),
            presets: vec![preset],
        }
    }
}
impl RemoteVoiceConfig {
    pub fn validate(&self) -> Result<(), RemoteVoiceError> {
        if self.version != 1 || self.presets.is_empty() || self.presets.len() > 32 {
            return Err(RemoteVoiceError::InvalidPreset(
                "invalid catalog version or size".into(),
            ));
        }
        let mut seen = HashSet::new();
        for preset in &self.presets {
            preset.validate()?;
            if !seen.insert(&preset.id) {
                return Err(RemoteVoiceError::InvalidPreset(
                    "duplicate preset ID".into(),
                ));
            }
        }
        if !seen.contains(&self.active_id) {
            return Err(RemoteVoiceError::InvalidPreset(
                "active preset is missing".into(),
            ));
        }
        Ok(())
    }
    pub fn active(&self) -> Result<&RemoteVoicePreset, RemoteVoiceError> {
        self.presets
            .iter()
            .find(|p| p.id == self.active_id)
            .ok_or_else(|| RemoteVoiceError::InvalidPreset("active preset is missing".into()))
    }
    pub fn add(&mut self) -> Result<(), RemoteVoiceError> {
        if self.presets.len() >= 32 {
            return Err(RemoteVoiceError::InvalidPreset("maximum 32 presets".into()));
        }
        let mut preset = RemoteVoicePreset::fresh();
        let name = preset.name.clone();
        let mut suffix = 2;
        while self.presets.iter().any(|p| p.name == preset.name) {
            preset.name = format!("{name} {suffix}");
            suffix += 1;
        }
        self.active_id = preset.id.clone();
        self.presets.push(preset);
        Ok(())
    }
    pub fn select(&mut self, id: &str) -> Result<(), RemoteVoiceError> {
        if !self.presets.iter().any(|p| p.id == id) {
            return Err(RemoteVoiceError::InvalidPreset(
                "preset does not exist".into(),
            ));
        }
        self.active_id = id.into();
        Ok(())
    }
    pub fn update(&mut self, preset: RemoteVoicePreset) -> Result<(), RemoteVoiceError> {
        preset.validate()?;
        let old = self
            .presets
            .iter_mut()
            .find(|p| p.id == preset.id)
            .ok_or_else(|| RemoteVoiceError::InvalidPreset("preset does not exist".into()))?;
        *old = preset;
        Ok(())
    }
    pub fn delete_active(&mut self) -> Result<(), RemoteVoiceError> {
        if self.presets.len() == 1 {
            return Err(RemoteVoiceError::InvalidPreset(
                "cannot delete the last preset".into(),
            ));
        }
        self.presets.retain(|p| p.id != self.active_id);
        self.active_id = self.presets[0].id.clone();
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum RemoteVoiceError {
    #[error("invalid voice preset: {0}")]
    InvalidPreset(String),
    #[error("voice unavailable: {0}")]
    VoiceUnavailable(String),
    #[error("Fish Audio authentication failed")]
    Authentication,
    #[error("Fish Audio balance insufficient")]
    Balance,
    #[error("Fish Audio rate limit exceeded")]
    RateLimit,
    #[error("Fish Audio rejected this request")]
    Rejected,
    #[error("Fish Audio service unavailable (HTTP {0})")]
    Http(u16),
    #[error("Fish Audio network error")]
    Network,
    #[error("Fish Audio response was not valid PCM16 audio")]
    InvalidAudio,
    #[error("Fish Audio response too large")]
    TooLarge,
    #[error("Fish Audio request timed out")]
    Timeout,
    #[error("voice operation cancelled")]
    Cancelled,
    #[error("voice storage I/O: {0}")]
    Storage(#[from] io::Error),
}

fn normalize_voice_id(value: &str) -> Result<String, RemoteVoiceError> {
    let id = if value.starts_with("https://") {
        let url = reqwest::Url::parse(value)
            .map_err(|_| RemoteVoiceError::VoiceUnavailable("invalid voice URL".into()))?;
        if !matches!(url.host_str(), Some("fish.audio" | "www.fish.audio"))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(RemoteVoiceError::VoiceUnavailable(
                "only fish.audio voice URLs are allowed".into(),
            ));
        }
        url.path_segments()
            .and_then(|mut parts| parts.find(|part| valid_hex_id(part)))
            .ok_or_else(|| RemoteVoiceError::VoiceUnavailable("voice URL has no ID".into()))?
            .to_owned()
    } else {
        value.to_owned()
    };
    if valid_hex_id(&id) {
        Ok(id.to_ascii_lowercase())
    } else {
        Err(RemoteVoiceError::VoiceUnavailable(
            "voice ID must be 32 hexadecimal characters".into(),
        ))
    }
}
fn valid_hex_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Separate from AppSettings/export. Secrets are not encrypted; Unix files use owner-only mode.
#[derive(Clone)]
pub struct RemoteVoiceStore {
    pub path: PathBuf,
}
impl RemoteVoiceStore {
    pub fn load(&self) -> Result<RemoteVoiceConfig, RemoteVoiceError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RemoteVoiceConfig::default());
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() > 128 * 1024 {
            return Err(RemoteVoiceError::InvalidPreset(
                "voice catalog too large".into(),
            ));
        }
        let config: RemoteVoiceConfig = serde_json::from_slice(&bytes)
            .map_err(|_| RemoteVoiceError::InvalidPreset("invalid voice catalog JSON".into()))?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self, config: &RemoteVoiceConfig) -> Result<(), RemoteVoiceError> {
        config.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| RemoteVoiceError::InvalidPreset("no catalog directory".into()))?;
        fs::create_dir_all(parent)?;
        let temp = self
            .path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), RemoteVoiceError> {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            serde_json::to_writer(&mut file, config).map_err(|_| {
                RemoteVoiceError::InvalidPreset("cannot serialize voice catalog".into())
            })?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

fn classify_http(status: reqwest::StatusCode) -> RemoteVoiceError {
    match status.as_u16() {
        401 | 403 => RemoteVoiceError::Authentication,
        402 => RemoteVoiceError::Balance,
        404 => RemoteVoiceError::VoiceUnavailable("voice not found".into()),
        422 => RemoteVoiceError::Rejected,
        429 => RemoteVoiceError::RateLimit,
        code => RemoteVoiceError::Http(code),
    }
}
fn pcm_to_wav(pcm: &[u8]) -> Result<AudioArtifact, RemoteVoiceError> {
    if pcm.is_empty() || !pcm.len().is_multiple_of(2) || pcm.len() > MAX_PCM {
        return Err(RemoteVoiceError::InvalidAudio);
    }
    let len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(len + 36).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&44_100_u32.to_le_bytes());
    wav.extend_from_slice(&88_200_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&len.to_le_bytes());
    wav.extend_from_slice(pcm);
    Ok(AudioArtifact {
        format: AudioFormat::Wave,
        bytes: Arc::from(wav),
    })
}

pub struct FishAudioClient {
    client: reqwest::Client,
    endpoint: String,
}
impl FishAudioClient {
    pub fn new() -> Result<Self, RemoteVoiceError> {
        Self::endpoint(ENDPOINT)
    }
    fn endpoint(endpoint: &str) -> Result<Self, RemoteVoiceError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| RemoteVoiceError::Network)?;
        Ok(Self {
            client,
            endpoint: endpoint.into(),
        })
    }
    pub async fn synthesize(
        &self,
        preset: &RemoteVoicePreset,
        text: &str,
        character: Option<CharacterId>,
        cancel: &CancellationToken,
    ) -> Result<AudioArtifact, RemoteVoiceError> {
        preset.validate()?;
        if preset.api_key.is_empty() {
            return Err(RemoteVoiceError::InvalidPreset(
                "API key is not configured".into(),
            ));
        }
        if text.trim().is_empty() || text.len() > 10_000 {
            return Err(RemoteVoiceError::InvalidPreset(
                "preview text must be 1..10000 bytes".into(),
            ));
        }
        let voice = preset.voice_for(character)?;
        let request = serde_json::json!({
            "text": text, "reference_id": voice, "format": "pcm",
            "sample_rate": 44100, "prosody": {"speed": f64::from(preset.speed_percent) / 100.0},
            "normalize_loudness": true
        });
        let work = async {
            let mut response = self
                .client
                .post(&self.endpoint)
                .header("Authorization", format!("Bearer {}", preset.api_key))
                .header("model", &preset.model)
                .json(&request)
                .send()
                .await
                .map_err(|_| RemoteVoiceError::Network)?;
            if !response.status().is_success() {
                return Err(classify_http(response.status()));
            }
            if response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value.starts_with("application/json") || value.starts_with("text/")
                })
            {
                return Err(RemoteVoiceError::InvalidAudio);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| RemoteVoiceError::Network)?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_PCM {
                    return Err(RemoteVoiceError::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            pcm_to_wav(&bytes)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(RemoteVoiceError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(180), work) => {
                result.map_err(|_| RemoteVoiceError::Timeout)?
            }
        }
    }
}

/// Routes production TTS to the active Fish Audio preset only when the user
/// selected the API backend. Other backends retain their existing voice engine.
pub(crate) struct RemoteVoiceRouter {
    pub local: Arc<dyn crate::voice::VoiceProvider>,
    pub settings: Arc<crate::settings::SettingsOwner>,
    pub store: RemoteVoiceStore,
    pub client: FishAudioClient,
}

impl crate::voice::VoiceProvider for RemoteVoiceRouter {
    fn synthesize(
        &self,
        text: Arc<str>,
        options: crate::voice::VoiceOptions,
    ) -> crate::voice::VoiceFuture<'_> {
        Box::pin(async move {
            use crate::voice::VoiceError;
            let current = self
                .settings
                .snapshot_all()
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            if current.values.speech.tts.backend != crate::settings::TtsBackend::Api {
                return self.local.synthesize(text, options).await;
            }
            if !current.values.speech.tts.enabled {
                return Err(VoiceError::Unavailable);
            }
            // Do not cache the credential or voice: every generation observes a
            // complete atomic catalog revision, including after a character switch.
            let store = self.store.clone();
            let config = tokio::task::spawn_blocking(move || store.load())
                .await
                .map_err(|_| VoiceError::Generation("voice catalog task stopped".into()))?
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            let preset = config
                .active()
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            let stop = CancellationToken::new();
            self.client
                .synthesize(preset, &text, options.character, &stop)
                .await
                .map_err(|error| VoiceError::Generation(error.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_prevents_leaking_character_voices_across_presets() {
        let mut cfg = RemoteVoiceConfig::default();
        cfg.add().unwrap();
        let second = cfg.active().unwrap().id.clone();
        cfg.active_id = "fish_audio_default".into();
        cfg.presets[0].default_voice_id = "a".repeat(32);
        cfg.presets[0]
            .character_voice_ids
            .insert(CharacterId::Crazy, "b".repeat(32));
        cfg.select(&second).unwrap();
        assert!(matches!(
            cfg.active().unwrap().voice_for(Some(CharacterId::Crazy)),
            Err(RemoteVoiceError::VoiceUnavailable(_))
        ));
        assert!(cfg.delete_active().is_ok());
        assert!(cfg.delete_active().is_err());
    }
    #[test]
    fn validates_urls_and_pcm_header() {
        assert_eq!(
            normalize_voice_id(&format!("https://fish.audio/model/{}", "A".repeat(32))).unwrap(),
            "a".repeat(32)
        );
        assert!(
            normalize_voice_id(&format!("https://evil.example/model/{}", "a".repeat(32))).is_err()
        );
        assert!(normalize_voice_id("not-a-voice").is_err());
        assert!(pcm_to_wav(&[1]).is_err());
        let wav = pcm_to_wav(&[0, 1, 2, 3]).unwrap();
        assert_eq!(&wav.bytes[0..4], b"RIFF");
        assert_eq!(&wav.bytes[40..44], &4_u32.to_le_bytes());
    }
    #[test]
    fn persists_presets_and_credentials_outside_app_settings() {
        let path = std::env::temp_dir().join(format!("nm-remote-{}.json", uuid::Uuid::new_v4()));
        let store = RemoteVoiceStore { path: path.clone() };
        let mut cfg = store.load().unwrap();
        cfg.presets[0].api_key = "test-secret-not-real".into();
        cfg.add().unwrap();
        store.save(&cfg).unwrap();
        let recovered = store.load().unwrap();
        assert_eq!(recovered.presets.len(), 2);
        assert_eq!(recovered.presets[0].api_key, "test-secret-not-real");
        let _ = fs::remove_file(path);
    }
    #[tokio::test]
    async fn mock_fish_pcm_and_auth_errors() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/tts", listener.local_addr().unwrap());
        let handler = tokio::spawn(async move {
            for status in ["200 OK", "401 Unauthorized"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 8192];
                let size = stream.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                assert!(request.contains("POST /v1/tts"));
                assert!(
                    request.contains("authorization: Bearer test-key")
                        || request.contains("Authorization: Bearer test-key")
                );
                let content: &[u8] = if status.starts_with("200") {
                    &[0, 1, 2, 3]
                } else {
                    b"{}"
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/octet-stream\r\n\r\n",
                    content.len()
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
                stream.write_all(content).await.unwrap();
            }
        });
        let client = FishAudioClient::endpoint(&url).unwrap();
        let mut preset = RemoteVoicePreset::fresh();
        preset.api_key = "test-key".into();
        preset.default_voice_id = "a".repeat(32);
        let stop = CancellationToken::new();
        let wav = client.synthesize(&preset, "hi", None, &stop).await.unwrap();
        assert_eq!(&wav.bytes[44..], &[0, 1, 2, 3]);
        assert!(matches!(
            client.synthesize(&preset, "hi", None, &stop).await,
            Err(RemoteVoiceError::Authentication)
        ));
        handler.await.unwrap();
    }
}
