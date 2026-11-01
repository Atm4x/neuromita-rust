use std::{path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};

use super::{
    AppSettings, CaptureSettings, CharacterSettings, CharactersSettings, InterfaceSettings,
    OperationPresets, PresetId, PresetSettings, SettingsError, VoiceSettings,
};

pub const SETTINGS_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSettings {
    pub schema_version: u32,
    pub revision: u64,
    pub settings: AppSettings,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettingsV1 {
    schema_version: u32,
    revision: u64,
    settings: AppSettingsV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppSettingsV1 {
    #[serde(default)]
    presets: PresetSettingsV1,
    voice: VoiceSettings,
    capture: CaptureSettings,
    interface: InterfaceSettings,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetSettingsV1 {
    active: PresetId,
    #[serde(default)]
    characters: CharacterPresetAssignmentsV1,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CharacterPresetAssignmentsV1 {
    mita: OperationPresets,
    kind_mita: OperationPresets,
}

impl StoredSettingsV1 {
    fn upgrade(self) -> StoredSettings {
        StoredSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            revision: self.revision,
            settings: AppSettings {
                presets: PresetSettings {
                    active: self.settings.presets.active,
                },
                characters: CharactersSettings {
                    mita: CharacterSettings {
                        presets: self.settings.presets.characters.mita,
                    },
                    kind_mita: CharacterSettings {
                        presets: self.settings.presets.characters.kind_mita,
                    },
                },
                voice: self.settings.voice,
                capture: self.settings.capture,
                interface: self.settings.interface,
            },
        }
    }
}

pub trait SettingsRepository: Send + Sync + 'static {
    fn load(&self) -> Result<StoredSettings, SettingsError>;
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError>;
}

pub struct MemorySettingsRepository {
    value: Mutex<Option<StoredSettings>>,
}
impl MemorySettingsRepository {
    pub fn new() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }
}
impl Default for MemorySettingsRepository {
    fn default() -> Self {
        Self::new()
    }
}
impl SettingsRepository for MemorySettingsRepository {
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        self.value
            .lock()
            .expect("settings storage poisoned")
            .clone()
            .ok_or_else(|| SettingsError::Storage("no stored settings".into()))
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        *self.value.lock().expect("settings storage poisoned") = Some(settings.clone());
        Ok(())
    }
}

pub struct JsonSettingsRepository {
    path: PathBuf,
}
impl JsonSettingsRepository {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}
impl SettingsRepository for JsonSettingsRepository {
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        let bytes =
            std::fs::read(&self.path).map_err(|error| SettingsError::Storage(error.to_string()))?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| SettingsError::Storage(error.to_string()))?;
        let version = json
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| SettingsError::Storage("missing settings schema version".into()))?;
        let stored = match version {
            1 => {
                let legacy: StoredSettingsV1 = serde_json::from_value(json)
                    .map_err(|error| SettingsError::Storage(error.to_string()))?;
                if legacy.schema_version != 1 {
                    return Err(SettingsError::Storage("unsupported settings schema".into()));
                }
                legacy.upgrade()
            }
            2 => serde_json::from_value(json)
                .map_err(|error| SettingsError::Storage(error.to_string()))?,
            _ => return Err(SettingsError::Storage("unsupported settings schema".into())),
        };
        stored.settings.validate()?;
        Ok(stored)
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        use std::io::Write;
        let bytes = serde_json::to_vec_pretty(settings)
            .map_err(|error| SettingsError::Storage(error.to_string()))?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|error| SettingsError::Storage(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PresetSelection;
    use serde_json::{Value, json};
    use uuid::Uuid;

    #[test]
    fn legacy_character_assignments_migrate_without_changing_selections() {
        let path = std::env::temp_dir().join(format!(
            "neuromita-settings-migration-{}.json",
            Uuid::new_v4()
        ));
        let repo = JsonSettingsRepository::new(path.clone());
        let mut settings = AppSettings::initial_profile();
        settings.presets.active = PresetId(3);
        settings.characters.mita.presets.react = PresetSelection::Specific(PresetId(7));
        settings.characters.kind_mita.presets.reply = PresetSelection::Specific(PresetId(11));
        let old_assignments = json!({
            "mita": settings.characters.mita.presets,
            "kind_mita": settings.characters.kind_mita.presets,
        });
        let mut old_settings = serde_json::to_value(&settings).unwrap();
        old_settings.as_object_mut().unwrap().remove("characters");
        old_settings["presets"]["characters"] = old_assignments;
        let old_record = json!({
            "schema_version": 1,
            "revision": 9,
            "settings": old_settings,
        });
        std::fs::write(&path, serde_json::to_vec(&old_record).unwrap()).unwrap();

        let upgraded = repo.load().unwrap();
        assert_eq!(upgraded.schema_version, SETTINGS_SCHEMA_VERSION);
        assert_eq!(upgraded.revision, 9);
        assert_eq!(upgraded.settings, settings);
        repo.save(&upgraded).unwrap();

        let persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["schema_version"], 2);
        assert!(persisted["settings"]["presets"].get("characters").is_none());
        assert!(persisted["settings"]["characters"].is_object());
        assert_eq!(repo.load().unwrap().settings, settings);
        std::fs::remove_file(path).unwrap();
    }
}
