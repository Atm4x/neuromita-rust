use super::{AppSettings, Language, SettingsRepository, SettingsTransferError, StoredSettings};
use crate::migration::{Migration, MigrationError, StartupMigration};
use serde::{
    Deserialize,
    de::{MapAccess, Visitor},
};
use serde_json::Value;
use std::{collections::BTreeMap, fmt, sync::Arc};

struct PythonSettingsDocument(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for PythonSettingsDocument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DocumentVisitor;
        impl<'de> Visitor<'de> for DocumentVisitor {
            type Value = PythonSettingsDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a Python settings JSON object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<String, Value>()? {
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate Python settings key {key}"
                        )));
                    }
                }
                Ok(PythonSettingsDocument(values))
            }
        }
        deserializer.deserialize_map(DocumentVisitor)
    }
}

/// Strict current Rust settings decoder, not a historical upgrade chain.
pub struct RustSettingsMigration;
impl Migration<&[u8]> for RustSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;
    fn migrate(&self, bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        let document: Value = serde_json::from_slice(bytes)
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

#[derive(Debug, Clone, Default)]
struct PythonSettingsPatch {
    language: Option<Language>,
    hide_private: Option<bool>,
    reminders_enabled: Option<bool>,
    voice_language: Option<String>,
    local_voice_auto_load: Option<bool>,
    local_voice_lazy_init: Option<bool>,
}

/// A reviewed, non-mutating Python settings import plan. Unmapped values remain
/// in the untouched Python document and are never silently treated as defaults.
#[derive(Debug, Clone)]
pub struct PythonSettingsPreview {
    pub mapped_keys: Vec<String>,
    pub unmapped_keys: Vec<String>,
    patch: PythonSettingsPatch,
}

impl PythonSettingsPreview {
    /// Apply only mapped fields, preserving all other current Rust preferences.
    pub fn apply_to(
        &self,
        existing: &StoredSettings,
    ) -> Result<StoredSettings, SettingsTransferError> {
        let mut candidate = existing.clone();
        if let Some(language) = &self.patch.language {
            candidate.settings.interface.language = language.clone();
        }
        if let Some(value) = self.patch.hide_private {
            candidate.settings.interface.hide_private = value;
        }
        if let Some(value) = self.patch.reminders_enabled {
            candidate.settings.reminders.enabled = value;
        }
        if let Some(value) = &self.patch.voice_language {
            candidate.settings.speech.tts.local_language = value.clone();
        }
        if let Some(value) = self.patch.local_voice_auto_load {
            candidate.settings.speech.tts.local_auto_load = value;
        }
        if let Some(value) = self.patch.local_voice_lazy_init {
            candidate.settings.speech.tts.local_lazy_init = value;
        }
        candidate
            .settings
            .validate()
            .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))?;
        if candidate.settings != existing.settings {
            candidate.revision = existing
                .revision
                .checked_add(1)
                .ok_or_else(|| SettingsTransferError::InvalidValue("revision exhausted".into()))?;
        }
        Ok(candidate)
    }
}

pub struct PythonSettingsMigration;
impl PythonSettingsMigration {
    pub fn preview(bytes: &[u8]) -> Result<PythonSettingsPreview, SettingsTransferError> {
        let document: PythonSettingsDocument = serde_json::from_slice(bytes)
            .map_err(|error| SettingsTransferError::MalformedJson(error.to_string()))?;
        let values = &document.0;
        let mut patch = PythonSettingsPatch::default();
        let mut mapped_keys = Vec::new();
        let mut unmapped_keys = Vec::new();
        for (key, value) in values {
            match key.as_str() {
                "LANGUAGE" => {
                    patch.language = Some(
                        match value.as_str().map(str::to_ascii_uppercase).as_deref() {
                            Some("RU") => Language::Russian,
                            Some("EN") => Language::English,
                            _ => return Err(invalid_python(key)),
                        },
                    );
                }
                "HIDE_PRIVATE" => patch.hide_private = Some(python_bool(key, value)?),
                "REMINDERS_ENABLED" => {
                    patch.reminders_enabled = Some(python_bool(key, value)?);
                }
                "VOICE_LANGUAGE" => {
                    patch.voice_language = Some(match value.as_str() {
                        Some(language @ ("ru" | "en")) => language.to_owned(),
                        _ => return Err(invalid_python(key)),
                    });
                }
                "LOCAL_VOICE_LOAD_LAST" => {
                    patch.local_voice_auto_load = Some(python_bool(key, value)?);
                }
                "LOCAL_VOICE_INIT_ON_REQUEST" => {
                    patch.local_voice_lazy_init = Some(python_bool(key, value)?);
                }
                _ => {
                    unmapped_keys.push(key.clone());
                    continue;
                }
            }
            mapped_keys.push(key.clone());
        }
        Ok(PythonSettingsPreview {
            mapped_keys,
            unmapped_keys,
            patch,
        })
    }
}

impl Migration<&[u8]> for PythonSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;
    fn migrate(&self, bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        let preview = Self::preview(bytes)?;
        if preview.mapped_keys.is_empty() {
            return Err(SettingsTransferError::NotFullSettingsTransfer(
                "Python settings have no supported fields to import".into(),
            ));
        }
        if !preview.unmapped_keys.is_empty() {
            return Err(SettingsTransferError::UnmappedPythonFields(
                preview.unmapped_keys,
            ));
        }
        preview.apply_to(&StoredSettings {
            revision: 0,
            settings: AppSettings::initial_profile(),
        })
    }
}

fn invalid_python(key: &str) -> SettingsTransferError {
    SettingsTransferError::InvalidValue(format!("invalid Python settings value for {key}"))
}

fn python_bool(key: &str, value: &Value) -> Result<bool, SettingsTransferError> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::Number(number) if number.as_i64() == Some(0) => Ok(false),
        Value::Number(number) if number.as_i64() == Some(1) => Ok(true),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" | "y" | "t" => Ok(true),
            "0" | "false" | "no" | "off" | "n" | "f" | "" => Ok(false),
            _ => Err(invalid_python(key)),
        },
        _ => Err(invalid_python(key)),
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
