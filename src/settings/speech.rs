use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::SettingsError;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsBackend {
    #[default]
    Telegram,
    Local,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsPreferences {
    pub enabled: bool,
    pub backend: TtsBackend,
    pub telegram_auto_connect: bool,
    pub telegram_service: String,
    pub telegram_wait_seconds: u16,
    pub telegram_interval_seconds: u16,
    pub local_model_id: String,
    pub local_language: String,
    pub local_auto_load: bool,
    pub local_lazy_init: bool,
    pub api_profile: String,
    pub api_model: String,
    pub api_speed_percent: u16,
    pub default_voice_id: String,
    pub character_voice_ids: BTreeMap<crate::character::CharacterId, String>,
}

impl Default for TtsPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: TtsBackend::Telegram,
            telegram_auto_connect: false,
            telegram_service: "@silero_voice_bot".into(),
            telegram_wait_seconds: 12,
            telegram_interval_seconds: 2,
            local_model_id: String::new(),
            local_language: "ru".into(),
            local_auto_load: false,
            local_lazy_init: true,
            api_profile: String::new(),
            api_model: String::new(),
            api_speed_percent: 100,
            default_voice_id: String::new(),
            character_voice_ids: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AsrPreferences {
    pub recognizer_id: String,
    pub device_id: String,
    pub audio_backend: String,
    pub instant_send: bool,
    pub delayed_send: bool,
    pub merge_chat: bool,
    pub pause_while_speaking: bool,
    pub send_delay_ms: u32,
    pub input_mode: String,
    pub model_languages: BTreeMap<String, String>,
    pub chunk_size: u32,
    pub vad_threshold_milli: u16,
    pub silence_ms: u32,
    pub pre_buffer_ms: u32,
    pub max_speech_ms: u32,
    pub min_speech_ms: u32,
}

impl Default for AsrPreferences {
    fn default() -> Self {
        Self {
            recognizer_id: String::new(),
            device_id: String::new(),
            audio_backend: String::new(),
            instant_send: false,
            delayed_send: true,
            merge_chat: true,
            pause_while_speaking: false,
            send_delay_ms: 3000,
            input_mode: "radio".into(),
            model_languages: BTreeMap::new(),
            chunk_size: 512,
            vad_threshold_milli: 500,
            silence_ms: 600,
            pre_buffer_ms: 400,
            max_speech_ms: 30_000,
            min_speech_ms: 350,
        }
    }
}

use crate::ai_runtime::model_identity::AiModelIdentity;
pub use crate::ai_runtime::model_identity::WorkerImplementation;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechModelReference {
    pub model_id: String,
    pub implementation: WorkerImplementation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl From<SpeechModelReference> for AiModelIdentity {
    fn from(model: SpeechModelReference) -> Self {
        Self {
            model_id: model.model_id,
            implementation: model.implementation,
            revision: model.revision,
        }
    }
}

impl SpeechModelReference {
    pub fn python(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            implementation: WorkerImplementation::Python,
            revision: None,
        }
    }

    pub fn rust(model_id: impl Into<String>, manifest_sha256: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            implementation: WorkerImplementation::Rust,
            revision: Some(manifest_sha256.into()),
        }
    }

    /// Distinguishes same-named model implementations without changing the model name.
    pub fn variant_key(&self) -> String {
        match self.implementation {
            WorkerImplementation::Python => match &self.revision {
                Some(revision) => format!("python:{}@{revision}", self.model_id),
                None => format!("python:{}", self.model_id),
            },
            WorkerImplementation::Rust => format!(
                "rust:{}@{}",
                self.model_id,
                self.revision.as_deref().unwrap_or_default()
            ),
        }
    }

    fn validate(&self) -> Result<(), SettingsError> {
        let id = self.model_id.as_str();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(SettingsError::Validation("invalid model identity".into()));
        }
        if self.implementation == WorkerImplementation::Rust
            && !self.revision.as_ref().is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(SettingsError::Validation(
                "Rust model requires a canonical lowercase manifest SHA-256".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeechSettings {
    pub tts: TtsPreferences,
    pub asr: AsrPreferences,
    pub tts_model_options: BTreeMap<String, serde_json::Value>,
    pub asr_model_options: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tts_model: Option<SpeechModelReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr_model: Option<SpeechModelReference>,
}

impl SpeechSettings {
    pub fn select_model(
        &mut self,
        service: crate::ai_runtime::AiService,
        model: Option<SpeechModelReference>,
    ) -> Result<(), SettingsError> {
        if let Some(model) = &model {
            model.validate()?;
        }
        let id = model
            .as_ref()
            .map(|model| model.model_id.clone())
            .unwrap_or_default();
        match service {
            crate::ai_runtime::AiService::Tts => {
                self.tts.local_model_id = id;
                self.tts_model = model;
            }
            crate::ai_runtime::AiService::Asr => {
                self.asr.recognizer_id = id;
                self.asr_model = model;
            }
            _ => return Err(SettingsError::Validation("not a speech service".into())),
        }
        Ok(())
    }

    /// A model card supplies this identity; implementation is the card's metadata.
    pub fn model_for(&self, service: crate::ai_runtime::AiService) -> Option<SpeechModelReference> {
        let (selected, id) = match service {
            crate::ai_runtime::AiService::Tts => (&self.tts_model, &self.tts.local_model_id),
            crate::ai_runtime::AiService::Asr => (&self.asr_model, &self.asr.recognizer_id),
            _ => return None,
        };
        selected
            .clone()
            .or_else(|| (!id.is_empty()).then(|| SpeechModelReference::python(id.clone())))
    }
}

impl SpeechSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        for (model, id) in [
            (&self.tts_model, &self.tts.local_model_id),
            (&self.asr_model, &self.asr.recognizer_id),
        ] {
            if model.as_ref().is_some_and(|model| &model.model_id != id) {
                return Err(SettingsError::Validation(
                    "speech model identity conflicts with its preference ID".into(),
                ));
            }
        }
        for service in [
            crate::ai_runtime::AiService::Tts,
            crate::ai_runtime::AiService::Asr,
        ] {
            if let Some(model) = self.model_for(service) {
                model.validate()?;
            }
        }
        let t = &self.tts;
        if t.telegram_wait_seconds == 0
            || t.telegram_wait_seconds > 600
            || t.telegram_interval_seconds > 600
            || !(50..=200).contains(&t.api_speed_percent)
            || t.telegram_service.len() > 256
            || [
                t.local_model_id.as_str(),
                t.api_profile.as_str(),
                t.api_model.as_str(),
            ]
            .iter()
            .any(|value| value.len() > 128)
            || !matches!(t.local_language.as_str(), "ru" | "en" | "auto" | "")
            || t.default_voice_id.len() > 512
            || t.character_voice_ids.len() > crate::character::CharacterId::ALL.len()
            || t.character_voice_ids
                .iter()
                .any(|(_, value)| value.len() > 512)
        {
            return Err(SettingsError::Validation("invalid TTS preferences".into()));
        }
        for models in [&self.tts_model_options, &self.asr_model_options] {
            if models.len() > 64 {
                return Err(SettingsError::Validation(
                    "too many speech model options".into(),
                ));
            }
            for (plugin_id, options) in models {
                if options.get("language").is_some() {
                    return Err(SettingsError::Validation(
                        "language belongs to the typed language preference, not model overrides"
                            .into(),
                    ));
                }
                if plugin_id.is_empty()
                    || plugin_id.len() > 128
                    || !plugin_id.is_ascii()
                    || !plugin_id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
                    || !options.as_object().is_some_and(|fields| {
                        fields.len() <= 128
                            && fields.iter().all(|(key, value)| {
                                !key.is_empty()
                                    && key.len() <= 128
                                    && match value {
                                        serde_json::Value::String(text) => text.len() <= 4096,
                                        serde_json::Value::Bool(_)
                                        | serde_json::Value::Number(_)
                                        | serde_json::Value::Null => true,
                                        _ => false,
                                    }
                            })
                    })
                    || serde_json::to_vec(options).is_ok_and(|bytes| bytes.len() > 32_768)
                {
                    return Err(SettingsError::Validation(
                        "invalid speech model options".into(),
                    ));
                }
            }
        }
        for (plugin, language) in &self.asr.model_languages {
            if let Some(capability) =
                crate::ai_runtime::speech_languages::asr_language_capability(plugin)
                && !capability
                    .choices
                    .iter()
                    .any(|choice| choice.id == *language)
            {
                return Err(SettingsError::Validation(format!(
                    "unsupported ASR language {language} for {plugin}"
                )));
            }
        }
        let a = &self.asr;
        if a.recognizer_id.len() > 128
            || a.device_id.len() > 512
            || a.audio_backend.len() > 64
            || a.send_delay_ms > 120_000
            || a.chunk_size == 0
            || a.chunk_size > 65_536
            || a.vad_threshold_milli > 1000
            || a.silence_ms > 120_000
            || a.pre_buffer_ms > 30_000
            || !(1000..=600_000).contains(&a.max_speech_ms)
            || a.min_speech_ms > a.max_speech_ms
            || a.model_languages.len() > 64
            || a.model_languages
                .iter()
                .any(|(key, value)| key.len() > 128 || value.len() > 32)
            || !matches!(
                a.input_mode.as_str(),
                "radio" | "continuous" | "push_to_talk"
            )
        {
            return Err(SettingsError::Validation("invalid ASR preferences".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn typed_selection_updates_preferences_atomically_and_rejects_divergent_edits() {
        use crate::settings::{
            AppSettings, MemorySettingsRepository, SettingsConfig, SettingsLifecycle,
            SettingsSource,
        };
        use crate::{ai_runtime::AiService, presentation::SettingsPresenter};
        use std::sync::Arc;
        let lifecycle = SettingsLifecycle::prepare(SettingsConfig {
            source: SettingsSource::initial(AppSettings::initial_profile()),
            repository: Arc::new(MemorySettingsRepository::new()),
        })
        .await
        .unwrap();
        let owner = lifecycle.owner();
        let presenter = SettingsPresenter::new(Arc::clone(&owner));
        let model = SpeechModelReference::rust("shared", "a".repeat(64));
        presenter.asr.model.set(Some(model.clone())).unwrap();
        let before = owner.snapshot_all().unwrap();
        assert_eq!(before.values.speech.asr.recognizer_id, "shared");
        assert_eq!(
            before.values.speech.model_for(AiService::Asr),
            Some(model.clone())
        );
        let mut invalid = presenter.asr.preferences.get();
        invalid.recognizer_id = "different".into();
        assert!(presenter.asr.preferences.set(invalid).is_err());
        assert_eq!(owner.snapshot_all().unwrap().revision, before.revision);
        assert_eq!(presenter.asr.model.get(), Some(model));
        presenter.asr.model.set(None).unwrap();
        assert!(presenter.asr.preferences.get().recognizer_id.is_empty());
        assert_eq!(presenter.asr.model.get(), None);
        assert_eq!(presenter.tts.model.get(), None);
    }
    #[test]
    fn same_named_implementations_are_distinct_models() {
        let mut saved = SpeechSettings::default();
        saved.tts.local_model_id = "voice".into();
        saved.asr.recognizer_id = "recognizer".into();
        assert_eq!(
            saved.model_for(crate::ai_runtime::AiService::Tts),
            Some(SpeechModelReference::python("voice"))
        );
        let python = SpeechModelReference::python("voice");
        let rust = SpeechModelReference::rust("voice", "a".repeat(64));
        assert_ne!(python, rust);
        saved.tts_model = Some(rust.clone());
        saved.asr_model = Some(SpeechModelReference::python("recognizer"));
        let encoded = serde_json::to_value(&saved).unwrap();
        assert!(encoded.get("tts_engine").is_none());
        assert_eq!(
            serde_json::from_value::<SpeechSettings>(encoded)
                .unwrap()
                .model_for(crate::ai_runtime::AiService::Tts),
            Some(rust)
        );
        let mut legacy = serde_json::to_value(SpeechSettings::default()).unwrap();
        legacy["tts_engine"] = serde_json::json!({"engine":"native","model_id":"voice","manifest_sha256":"a".repeat(64)});
        assert!(serde_json::from_value::<SpeechSettings>(legacy).is_err());
    }
}
