use super::{
    GuideLevel, Language, SettingsRepository, SettingsTransfer, SettingsTransferError,
    StoredSettings, Volume,
};
use crate::migration::{Migration, MigrationError, StartupMigration};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt, sync::Arc};

/// Strict current Rust settings decoder, not a historical upgrade chain.
pub struct RustSettingsMigration;
impl Migration<&[u8]> for RustSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;
    fn migrate(&self, bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        let document: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| SettingsTransferError::MalformedJson(e.to_string()))?;
        let object = document.as_object().ok_or_else(|| {
            SettingsTransferError::NotFullSettingsTransfer("expected an envelope object".into())
        })?;
        for field in ["revision", "settings"] {
            if !object.contains_key(field) {
                return Err(SettingsTransferError::NotFullSettingsTransfer(format!(
                    "missing {field}"
                )));
            }
        }
        let stored: StoredSettings = serde_json::from_slice(bytes)
            .map_err(|e| SettingsTransferError::InvalidEnvelope(e.to_string()))?;
        stored
            .settings
            .validate()
            .map_err(|e| SettingsTransferError::InvalidValue(e.to_string()))?;
        Ok(stored)
    }
}

/// A selective, read-only preview of an existing Python `Settings/settings.json`.
/// Unmapped keys are never interpreted as default values or silently discarded.
#[derive(Debug, Clone)]
pub struct PythonSettingsPreview {
    pub transfer: SettingsTransfer,
    pub imported_keys: Vec<String>,
    pub skipped_keys: Vec<String>,
    pub changed: bool,
}

pub struct PythonSettingsMigration;
impl PythonSettingsMigration {
    pub fn preview(
        &self,
        bytes: &[u8],
        current: &StoredSettings,
    ) -> Result<PythonSettingsPreview, SettingsTransferError> {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(SettingsTransferError::InvalidValue(
                "Python settings exceed the 8 MiB import limit".into(),
            ));
        }
        let PythonSettingsObject(object) = serde_json::from_slice(bytes)
            .map_err(|error| SettingsTransferError::MalformedJson(error.to_string()))?;
        if object.contains_key("settings") && object.contains_key("revision") {
            return Err(SettingsTransferError::NotFullSettingsTransfer(
                "this is a Rust settings envelope, not Python settings".into(),
            ));
        }
        let mut proposed = current.clone();
        let mut imported_keys = Vec::new();
        let mut skipped_keys = Vec::new();
        for (key, value) in &object {
            match key.as_str() {
                "LANGUAGE" => {
                    let code = value
                        .as_str()
                        .ok_or_else(|| invalid_python(key, "expected RU or EN"))?;
                    proposed.settings.interface.language = match code.to_ascii_uppercase().as_str()
                    {
                        "RU" => Language::Russian,
                        "EN" => Language::English,
                        _ => return Err(invalid_python(key, "only RU and EN are supported")),
                    };
                }
                "HIDE_PRIVATE" => {
                    proposed.settings.interface.hide_private = python_bool(key, value)?;
                }
                "REMINDERS_ENABLED" => {
                    proposed.settings.reminders.enabled = python_bool(key, value)?;
                }
                "VOICE_LANGUAGE" => {
                    let language = value
                        .as_str()
                        .ok_or_else(|| invalid_python(key, "expected ru or en"))?
                        .trim()
                        .to_ascii_lowercase();
                    if !matches!(language.as_str(), "ru" | "en") {
                        return Err(invalid_python(key, "only ru and en are supported"));
                    }
                    proposed.settings.speech.tts.local_language = language;
                }
                "LOCAL_VOICE_LOAD_LAST" => {
                    proposed.settings.speech.tts.local_auto_load = python_bool(key, value)?;
                }
                "LOCAL_VOICE_INIT_ON_REQUEST" => {
                    proposed.settings.speech.tts.local_lazy_init = python_bool(key, value)?;
                }
                "GUIDE_LEVEL" => {
                    proposed.settings.onboarding.guide_level = match value.as_str() {
                        Some("basic") => GuideLevel::Basic,
                        Some("advanced") => GuideLevel::Advanced,
                        Some("full") => GuideLevel::Full,
                        _ => {
                            return Err(invalid_python(key, "expected basic, advanced or full"));
                        }
                    };
                }
                "EULA_ACCEPTED" => {
                    proposed.settings.onboarding.eula_accepted = value
                        .as_bool()
                        .ok_or_else(|| invalid_python(key, "expected boolean"))?;
                }
                "CHAT_FONT_SIZE" => {
                    let size = python_u64(value, key, "expected integer in range 6..=96")?;
                    proposed.settings.interface.font_size = u16::try_from(size)
                        .ok()
                        .filter(|size| (6..=96).contains(size))
                        .ok_or_else(|| invalid_python(key, "expected integer in range 6..=96"))?;
                }
                "VOICEOVER_LOCAL_VOLUME" => {
                    let volume = python_u64(value, key, "expected integer in range 0..=100")?;
                    let volume = u8::try_from(volume)
                        .map_err(|_| invalid_python(key, "expected integer in range 0..=100"))?;
                    proposed.settings.voice.volume = Volume::new(volume)
                        .map_err(|_| invalid_python(key, "expected integer in range 0..=100"))?;
                }
                _ if key.starts_with("PROMPT_SET_") => {
                    let character_name = &key["PROMPT_SET_".len()..];
                    if let Some(character) = crate::character::CharacterId::parse(character_name) {
                        let set = if value.is_null() {
                            ""
                        } else {
                            value
                                .as_str()
                                .ok_or_else(|| invalid_python(key, "expected prompt-set name"))?
                        };
                        if set.is_empty() {
                            proposed
                                .settings
                                .characters
                                .for_character_mut(character)
                                .prompt_set = None;
                        } else {
                            crate::prompts::PromptCatalog::validate_set_name(set)
                                .map_err(|_| invalid_python(key, "invalid prompt-set name"))?;
                            proposed
                                .settings
                                .characters
                                .for_character_mut(character)
                                .prompt_set = Some(set.into());
                        }
                    } else {
                        skipped_keys.push(key.clone());
                        continue;
                    }
                }
                _ => {
                    skipped_keys.push(key.clone());
                    continue;
                }
            }
            imported_keys.push(key.clone());
        }
        if imported_keys.is_empty() {
            return Err(SettingsTransferError::NotFullSettingsTransfer(
                "no supported Python settings keys were found".into(),
            ));
        }
        proposed
            .settings
            .validate()
            .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))?;
        let changed = proposed.settings != current.settings;
        Ok(PythonSettingsPreview {
            transfer: SettingsTransfer::new(proposed)?,
            imported_keys,
            skipped_keys,
            changed,
        })
    }
}

struct PythonSettingsObject(BTreeMap<String, serde_json::Value>);

impl<'de> Deserialize<'de> for PythonSettingsObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FlatObject;
        impl<'de> Visitor<'de> for FlatObject {
            type Value = PythonSettingsObject;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a flat Python settings JSON object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, serde_json::Value>()? {
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate Python setting {key}"
                        )));
                    }
                }
                Ok(PythonSettingsObject(values))
            }
        }
        deserializer.deserialize_map(FlatObject)
    }
}

fn python_u64(
    value: &serde_json::Value,
    key: &str,
    message: &str,
) -> Result<u64, SettingsTransferError> {
    value
        .as_u64()
        .or_else(|| {
            let text = value.as_str()?.trim();
            if text.is_empty() || !text.bytes().all(|digit| digit.is_ascii_digit()) {
                return None;
            }
            text.parse().ok()
        })
        .ok_or_else(|| invalid_python(key, message))
}

fn python_bool(key: &str, value: &serde_json::Value) -> Result<bool, SettingsTransferError> {
    match value {
        serde_json::Value::Bool(value) => Ok(*value),
        serde_json::Value::Number(number) if number.as_u64() == Some(0) => Ok(false),
        serde_json::Value::Number(number) if number.as_u64() == Some(1) => Ok(true),
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" | "y" | "t" => Ok(true),
            "0" | "false" | "no" | "off" | "n" | "f" | "" => Ok(false),
            _ => Err(invalid_python(key, "expected a Python boolean")),
        },
        _ => Err(invalid_python(key, "expected a Python boolean")),
    }
}

fn invalid_python(key: &str, message: &str) -> SettingsTransferError {
    SettingsTransferError::InvalidValue(format!("Python {key}: {message}"))
}

impl Migration<&[u8]> for PythonSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;
    fn migrate(&self, _bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        Err(SettingsTransferError::PythonNeedsTarget)
    }
}

/// Keep the generic startup migration slot, but only validate current settings.
pub(crate) struct SettingsStartupMigration(pub Arc<dyn SettingsRepository>);
impl StartupMigration for SettingsStartupMigration {
    fn name(&self) -> &str {
        "settings"
    }
    fn run(&self) -> Result<(), MigrationError> {
        self.0
            .migrate()
            .map_err(|e| MigrationError::Storage(e.to_string()))
    }
}
