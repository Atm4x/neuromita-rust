//! Fish Audio cloud voice presets. Credentials are stored separately from exported AppSettings.
//! The storage file is access-restricted on Unix but is not encrypted at rest.
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{sync::watch, task};
use tokio_util::sync::CancellationToken;

use crate::{
    audio::device::{AudioPlayback, FfplayOutput},
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
    /// Build a validated preview snapshot. A stale or invalid editor must not
    /// replay the previously saved voice (possibly belonging to another Mita).
    pub fn prepared_preview(
        &self,
        preset: RemoteVoicePreset,
        character: Option<CharacterId>,
    ) -> Result<Self, RemoteVoiceError> {
        if self.active_id != preset.id {
            return Err(RemoteVoiceError::InvalidPreset(
                "selected voice preset changed; reload before preview".into(),
            ));
        }
        if preset.api_key.is_empty() {
            return Err(RemoteVoiceError::InvalidPreset(
                "API key is not configured".into(),
            ));
        }
        preset.voice_for(character)?;
        let mut next = self.clone();
        next.update(preset)?;
        Ok(next)
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

/// Shared catalog and preview owner for GUI, MCP and production API voice routing.
/// Observer state never contains credentials, source text or audio.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteVoiceAdminSnapshot {
    pub revision: u64,
    pub active_id: String,
    pub presets: Vec<RemoteVoicePresetSummary>,
    pub phase: String,
    pub busy: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteVoicePresetSummary {
    pub id: String,
    pub name: String,
    pub template_id: String,
    pub model: String,
    pub speed_percent: u16,
    pub default_voice_id: String,
    pub character_voice_ids: BTreeMap<CharacterId, String>,
    pub credential_present: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteVoicePresetPatch {
    pub id: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub speed_percent: Option<u16>,
    pub default_voice_id: Option<String>,
    pub character_voice_ids: Option<BTreeMap<CharacterId, String>>,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for RemoteVoicePresetPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteVoicePresetPatch")
            .field("id", &self.id)
            .field("has_credential_update", &self.api_key.is_some())
            .finish_non_exhaustive()
    }
}

impl RemoteVoicePresetPatch {
    pub fn has_secret(&self) -> bool {
        self.api_key.is_some()
    }

    fn apply_to(self, preset: &mut RemoteVoicePreset) -> Result<(), RemoteVoiceError> {
        if preset.id != self.id {
            return Err(RemoteVoiceError::InvalidPreset("preset changed".into()));
        }
        if let Some(value) = self.name {
            preset.name = value;
        }
        if let Some(value) = self.model {
            preset.model = value;
        }
        if let Some(value) = self.speed_percent {
            preset.speed_percent = value;
        }
        if let Some(value) = self.default_voice_id {
            preset.default_voice_id = value;
        }
        if let Some(value) = self.character_voice_ids {
            preset.character_voice_ids = value;
        }
        if let Some(value) = self.api_key {
            preset.api_key = value;
        }
        preset.validate()
    }
}

#[derive(Clone)]
pub enum RemoteVoiceCatalogAction {
    Add,
    Delete,
    Select(String),
    SelectWithSave(String, RemoteVoicePreset),
    SelectWithPatch(String, RemoteVoicePresetPatch),
    Save(RemoteVoicePreset),
    Patch(RemoteVoicePresetPatch),
}

struct RemoteVoiceAdminState {
    config: RemoteVoiceConfig,
    revision: u64,
    generation: u64,
    preview: Option<CancellationToken>,
    phase: &'static str,
}

pub struct RemoteVoiceAdmin {
    store: RemoteVoiceStore,
    state: Mutex<RemoteVoiceAdminState>,
    changes: watch::Sender<RemoteVoiceAdminSnapshot>,
}

impl RemoteVoiceAdmin {
    pub fn new(store: RemoteVoiceStore) -> Result<Self, RemoteVoiceError> {
        let config = store.load()?;
        let state = RemoteVoiceAdminState {
            config,
            revision: 0,
            generation: 0,
            preview: None,
            phase: "idle",
        };
        let (changes, _) = watch::channel(Self::view(&state));
        Ok(Self {
            store,
            state: Mutex::new(state),
            changes,
        })
    }

    fn view(state: &RemoteVoiceAdminState) -> RemoteVoiceAdminSnapshot {
        RemoteVoiceAdminSnapshot {
            revision: state.revision,
            active_id: state.config.active_id.clone(),
            presets: state
                .config
                .presets
                .iter()
                .map(|p| RemoteVoicePresetSummary {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    template_id: p.template_id.clone(),
                    model: p.model.clone(),
                    speed_percent: p.speed_percent,
                    default_voice_id: p.default_voice_id.clone(),
                    character_voice_ids: p.character_voice_ids.clone(),
                    credential_present: !p.api_key.is_empty(),
                })
                .collect(),
            phase: state.phase.to_string(),
            busy: state.preview.is_some(),
        }
    }

    pub fn snapshot(&self) -> RemoteVoiceAdminSnapshot {
        Self::view(&self.state.lock().expect("remote voice admin poisoned"))
    }

    pub fn config(&self) -> RemoteVoiceConfig {
        self.state
            .lock()
            .expect("remote voice admin poisoned")
            .config
            .clone()
    }

    pub fn observe(&self) -> watch::Receiver<RemoteVoiceAdminSnapshot> {
        self.changes.subscribe()
    }

    fn publish(&self, state: &mut RemoteVoiceAdminState) -> RemoteVoiceAdminSnapshot {
        state.revision += 1;
        let snapshot = Self::view(state);
        self.changes.send_replace(snapshot.clone());
        snapshot
    }

    pub fn change(
        &self,
        action: RemoteVoiceCatalogAction,
        expected_revision: Option<u64>,
    ) -> Result<RemoteVoiceAdminSnapshot, RemoteVoiceError> {
        let mut state = self.state.lock().expect("remote voice admin poisoned");
        if expected_revision.is_some_and(|revision| revision != state.revision) {
            return Err(RemoteVoiceError::InvalidPreset(
                "catalog revision changed".into(),
            ));
        }
        let mut next = state.config.clone();
        match action {
            RemoteVoiceCatalogAction::Add => next.add()?,
            RemoteVoiceCatalogAction::Delete => next.delete_active()?,
            RemoteVoiceCatalogAction::Select(id) => next.select(&id)?,
            RemoteVoiceCatalogAction::SelectWithSave(id, preset) => {
                next.update(preset)?;
                next.select(&id)?;
            }
            RemoteVoiceCatalogAction::SelectWithPatch(id, patch) => {
                let p = next
                    .presets
                    .iter_mut()
                    .find(|p| p.id == patch.id)
                    .ok_or_else(|| {
                        RemoteVoiceError::InvalidPreset("preset does not exist".into())
                    })?;
                patch.apply_to(p)?;
                next.select(&id)?;
            }
            RemoteVoiceCatalogAction::Save(preset) => next.update(preset)?,
            RemoteVoiceCatalogAction::Patch(patch) => {
                let p = next
                    .presets
                    .iter_mut()
                    .find(|p| p.id == patch.id)
                    .ok_or_else(|| {
                        RemoteVoiceError::InvalidPreset("preset does not exist".into())
                    })?;
                patch.apply_to(p)?;
            }
        }
        self.store.save(&next)?;
        if let Some(cancel) = state.preview.take() {
            cancel.cancel();
        }
        state.generation += 1;
        state.config = next;
        state.phase = "saved";
        Ok(self.publish(&mut state))
    }

    pub fn ffplay_binary(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) {
            "ffplay.exe"
        } else {
            "ffplay"
        };
        [
            std::env::var_os("NEUROMITA_FFPLAY").map(PathBuf::from),
            self.store
                .path
                .parent()
                .map(|dir| dir.join("AIData/bin").join(name)),
        ]
        .into_iter()
        .flatten()
        .find(|p| p.is_absolute() && p.is_file())
    }

    pub fn preview_patch(
        self: &Arc<Self>,
        text: String,
        character: Option<CharacterId>,
        patch: RemoteVoicePresetPatch,
        ffplay: PathBuf,
        expected_revision: u64,
    ) -> Result<RemoteVoiceAdminSnapshot, RemoteVoiceError> {
        let config = self.config();
        let preset = config
            .presets
            .iter()
            .find(|p| p.id == patch.id)
            .ok_or_else(|| RemoteVoiceError::InvalidPreset("preset does not exist".into()))?;
        let mut draft = preset.clone();
        patch.apply_to(&mut draft)?;
        self.preview(text, character, draft, ffplay, Some(expected_revision))
    }

    pub fn cancel(&self) -> RemoteVoiceAdminSnapshot {
        let mut state = self.state.lock().expect("remote voice admin poisoned");
        if let Some(cancel) = state.preview.take() {
            cancel.cancel();
            state.generation += 1;
            state.phase = "cancelled";
            self.publish(&mut state)
        } else {
            Self::view(&state)
        }
    }

    /// Start a single preview. A completed older task must never clear or overwrite newer state.
    pub fn preview(
        self: &Arc<Self>,
        text: String,
        character: Option<CharacterId>,
        draft: RemoteVoicePreset,
        ffplay: PathBuf,
        expected_revision: Option<u64>,
    ) -> Result<RemoteVoiceAdminSnapshot, RemoteVoiceError> {
        if text.trim().is_empty() || text.len() > 4096 {
            return Err(RemoteVoiceError::InvalidPreset(
                "invalid preview text length".into(),
            ));
        }
        if !ffplay.is_absolute() || !ffplay.is_file() {
            return Err(RemoteVoiceError::InvalidPreset(
                "preview playback executable unavailable".into(),
            ));
        }
        let (preset, generation, stop, snapshot) = {
            let mut state = self.state.lock().expect("remote voice admin poisoned");
            if expected_revision.is_some_and(|revision| revision != state.revision) {
                return Err(RemoteVoiceError::InvalidPreset(
                    "catalog revision changed".into(),
                ));
            }
            if state.preview.is_some() {
                return Err(RemoteVoiceError::InvalidPreset(
                    "preview already running".into(),
                ));
            }
            let next = state.config.prepared_preview(draft, character)?;
            let preset = next.active()?.clone();
            self.store.save(&next)?;
            state.config = next;
            state.generation += 1;
            let generation = state.generation;
            let stop = CancellationToken::new();
            state.preview = Some(stop.clone());
            state.phase = "previewing";
            let snapshot = self.publish(&mut state);
            (preset, generation, stop, snapshot)
        };
        let admin = Arc::clone(self);
        task::spawn(async move {
            let result = async {
                let client = FishAudioClient::new()?;
                let audio = client.synthesize(&preset, &text, character, &stop).await?;
                FfplayOutput { binary: ffplay }
                    .play(audio, stop.clone())
                    .await
                    .map_err(|_| RemoteVoiceError::InvalidAudio)?;
                if stop.is_cancelled() {
                    Err(RemoteVoiceError::Cancelled)
                } else {
                    Ok(())
                }
            }
            .await;
            let mut state = admin.state.lock().expect("remote voice admin poisoned");
            if state.generation == generation {
                state.preview = None;
                state.phase = match result {
                    Ok(()) => "completed",
                    Err(RemoteVoiceError::Cancelled) => "cancelled",
                    Err(_) => "failed",
                };
                admin.publish(&mut state);
            }
        });
        Ok(snapshot)
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
            "sample_rate": 44100,
            "prosody": {"speed": f64::from(preset.speed_percent) / 100.0, "normalize_loudness": true}
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
                .map_err(|error| {
                    if error.is_timeout() {
                        RemoteVoiceError::Timeout
                    } else {
                        RemoteVoiceError::Network
                    }
                })?;
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
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    RemoteVoiceError::Timeout
                } else {
                    RemoteVoiceError::Network
                }
            })? {
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
    pub admin: Arc<RemoteVoiceAdmin>,
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
            let config = self.admin.config();
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
    fn administration_catalog_is_cas_guarded_and_never_exports_credentials() {
        let root =
            std::env::temp_dir().join(format!("remote-voice-admin-test-{}", uuid::Uuid::new_v4()));
        let admin = RemoteVoiceAdmin::new(RemoteVoiceStore {
            path: root.join("remote_voice.json"),
        })
        .unwrap();
        let initial = admin.snapshot();
        assert_eq!(initial.revision, 0);
        assert_eq!(initial.presets.len(), 1);
        let id = initial.active_id.clone();
        let patch = RemoteVoicePresetPatch {
            id,
            name: Some("Voice A".into()),
            model: None,
            speed_percent: Some(135),
            default_voice_id: None,
            character_voice_ids: None,
            api_key: Some("secret-Fish-API-key-XYZ".into()),
        };
        let next = admin
            .change(RemoteVoiceCatalogAction::Patch(patch.clone()), Some(0))
            .unwrap();
        assert_eq!(next.revision, 1);
        assert!(next.presets[0].credential_present);
        assert_eq!(next.presets[0].name, "Voice A");
        assert!(
            !serde_json::to_string(&next)
                .unwrap()
                .contains("secret-Fish-API-key-XYZ")
        );
        assert!(!format!("{patch:?}").contains("secret-Fish-API-key-XYZ"));
        assert!(
            admin
                .change(RemoteVoiceCatalogAction::Add, Some(0))
                .is_err()
        );
        assert_eq!(admin.snapshot().revision, 1);
        let cloned = admin.config();
        assert_eq!(cloned.active().unwrap().api_key, "secret-Fish-API-key-XYZ");
        let second = RemoteVoiceAdmin::new(RemoteVoiceStore {
            path: root.join("remote_voice.json"),
        })
        .unwrap();
        assert_eq!(second.config().active().unwrap().name, "Voice A");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_preview_keeps_catalog_and_cancel_has_observable_revision() {
        let root = std::env::temp_dir().join(format!(
            "remote-voice-preview-test-{}",
            uuid::Uuid::new_v4()
        ));
        let admin = Arc::new(
            RemoteVoiceAdmin::new(RemoteVoiceStore {
                path: root.join("remote_voice.json"),
            })
            .unwrap(),
        );
        let patch = RemoteVoicePresetPatch {
            id: admin.snapshot().active_id,
            name: None,
            model: None,
            speed_percent: None,
            default_voice_id: None,
            character_voice_ids: None,
            api_key: None,
        };
        let player = std::env::current_exe().unwrap();
        assert!(
            admin
                .preview_patch("hello".into(), None, patch, player, 0)
                .is_err()
        );
        assert_eq!(admin.snapshot().revision, 0);
        let unchanged = admin.cancel();
        assert_eq!(unchanged.revision, 0);
        let token = CancellationToken::new();
        {
            let mut state = admin.state.lock().unwrap();
            state.preview = Some(token.clone());
            state.phase = "previewing";
            admin.publish(&mut state);
        }
        let after = admin.cancel();
        assert_eq!(after.revision, 2);
        assert_eq!(after.phase, "cancelled");
        assert!(!after.busy);
        assert!(token.is_cancelled());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn preview_rejects_stale_or_invalid_character_voice_without_fallback() {
        let mut config = RemoteVoiceConfig::default();
        config.presets[0].default_voice_id = "a".repeat(32);
        let mut draft = config.active().unwrap().clone();
        draft.api_key = "test-key".into();
        draft.default_voice_id = "b".repeat(32);
        let preview = config
            .prepared_preview(draft.clone(), Some(CharacterId::Crazy))
            .unwrap();
        assert_eq!(
            preview
                .active()
                .unwrap()
                .voice_for(Some(CharacterId::Crazy))
                .unwrap(),
            "b".repeat(32)
        );
        assert_eq!(
            config.active().unwrap().voice_for(None).unwrap(),
            "a".repeat(32)
        );
        let mut stale = draft.clone();
        stale.id = uuid::Uuid::new_v4().to_string();
        assert!(config.prepared_preview(stale, None).is_err());
        draft
            .character_voice_ids
            .insert(CharacterId::Crazy, "wrong-voice".into());
        assert!(
            config
                .prepared_preview(draft, Some(CharacterId::Crazy))
                .is_err()
        );
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
