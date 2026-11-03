use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::migration::{MigrationError, MigrationSchema, MigrationStore, VersionedData};
use serde::{Deserialize, Serialize};

use super::{AppSettings, SettingsError, SettingsImportFormat, SettingsTransfer};
use super::{RustSettingsMigration, migration::SettingsSchema};

pub const SETTINGS_SCHEMA_VERSION: u32 = 4;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSettings {
    pub schema_version: u32,
    pub revision: u64,
    pub settings: AppSettings,
}

pub trait SettingsRepository: Send + Sync + 'static {
    /// Completes required persisted upgrades before settings are published.
    fn migrate(&self) -> Result<(), SettingsError>;
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
    fn migrate(&self) -> Result<(), SettingsError> {
        SettingsTransfer::new(self.load()?)?;
        Ok(())
    }
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
    fn migrate(&self) -> Result<(), SettingsError> {
        RustSettingsMigration::service()
            .run(self)
            .map_err(|error| SettingsError::Storage(error.to_string()))?;
        Ok(())
    }
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

impl MigrationStore<Vec<u8>> for JsonSettingsRepository {
    fn read_versioned(&self) -> Result<VersionedData<Vec<u8>>, MigrationError> {
        let bytes = std::fs::read(&self.path)
            .map_err(|error| MigrationError::Storage(error.to_string()))?;
        RustSettingsMigration::document(bytes)
            .map_err(|error| MigrationError::InvalidData(error.to_string()))
    }

    fn commit(
        &self,
        expected: &VersionedData<Vec<u8>>,
        next: &VersionedData<Vec<u8>>,
    ) -> Result<(), MigrationError> {
        SettingsSchema.validate(next)?;
        if next.version <= expected.version {
            return Err(MigrationError::InvalidData(
                "migration must advance the schema".into(),
            ));
        }
        let _lock =
            writer_lock(&self.path).map_err(|error| MigrationError::Storage(error.to_string()))?;
        let current = self.read_versioned()?;
        if current != *expected {
            return Err(MigrationError::Conflict);
        }
        atomic_replace(&self.path, &next.data)
            .map_err(|error| MigrationError::Storage(error.to_string()))
    }
}

/// Serializes cooperating writers using a stable sidecar lock, then publishes
/// one complete file. Directory/power-loss durability is not promised.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let _lock = writer_lock(path)?;
    atomic_replace(path, bytes)
}

fn writer_lock(path: &Path) -> Result<std::fs::File, SettingsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_file_name(lock_name))
        .map_err(|error| SettingsError::Storage(error.to_string()))?;
    file.lock()
        .map_err(|error| SettingsError::Storage(error.to_string()))?;
    Ok(file)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
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
        settings.characters.crazy.presets.react = PresetSelection::Specific(PresetId(7));
        settings.characters.kind.presets.reply = PresetSelection::Specific(PresetId(11));
        let old_assignments = json!({
            "mita": settings.characters.crazy.presets,
            "kind_mita": settings.characters.kind.presets,
        });
        let mut old_settings = serde_json::to_value(&settings).unwrap();
        old_settings.as_object_mut().unwrap().remove("characters");
        old_settings.as_object_mut().unwrap().remove("reminders");
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
        assert_eq!(persisted["schema_version"], SETTINGS_SCHEMA_VERSION);
        assert!(persisted["settings"]["presets"].get("characters").is_none());
        assert!(persisted["settings"]["characters"].is_object());
        assert_eq!(repo.load().unwrap().settings, settings);
        std::fs::remove_file(path).unwrap();
    }
}
