use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{AppSettings, SettingsError, SettingsImportFormat, SettingsTransfer};

pub const SETTINGS_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSettings {
    pub schema_version: u32,
    pub revision: u64,
    pub settings: AppSettings,
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
        SettingsTransfer::new(settings.clone())?;
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

    pub fn export_to(&self, destination: &Path) -> Result<SettingsTransfer, SettingsError> {
        let transfer = SettingsTransfer::new(self.load()?)?;
        atomic_write(destination, &transfer.encode()?)?;
        Ok(transfer)
    }

    /// Offline replacement; the application using this file must be stopped.
    pub fn import(&self, bytes: &[u8]) -> Result<SettingsTransfer, SettingsError> {
        self.import_from(SettingsImportFormat::Rust, bytes)
    }

    pub fn import_from(
        &self,
        format: SettingsImportFormat,
        bytes: &[u8],
    ) -> Result<SettingsTransfer, SettingsError> {
        let transfer = SettingsTransfer::prepare(format, bytes)?;
        atomic_write(&self.path, &transfer.encode()?)?;
        Ok(transfer)
    }
}
impl SettingsRepository for JsonSettingsRepository {
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        let bytes =
            std::fs::read(&self.path).map_err(|error| SettingsError::Storage(error.to_string()))?;
        Ok(SettingsTransfer::decode(&bytes)?.into_stored())
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        let transfer = SettingsTransfer::new(settings.clone())?;
        atomic_write(&self.path, &transfer.encode()?)
    }
}

/// Publishes one complete file using a unique sibling temp. This is not an
/// interprocess lock and does not promise directory/power-loss durability.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temporary = path.with_file_name(temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| SettingsError::Storage(error.to_string()))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| SettingsError::Storage(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{PresetId, PresetSelection};
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
