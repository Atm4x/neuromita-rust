use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{AppSettings, SettingsError, SettingsTransfer};

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

/// JSON settings store.
///
/// Every write uses a fresh sibling temporary file, syncs and closes it, then
/// replaces the destination. Concurrent writers never share temporary bytes and
/// the last completed rename wins. Orphaned temporary files from an interrupted
/// save are ignored and left in place. Only the file itself is synced; no
/// directory or power-loss durability is promised.
pub struct JsonSettingsRepository {
    path: PathBuf,
}
impl JsonSettingsRepository {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Writes a complete backup of the current file to destination.
    ///
    /// The source is validated before the destination is touched. An existing
    /// destination is replaced atomically, never appended or merged.
    pub fn export_to(&self, destination: &Path) -> Result<SettingsTransfer, SettingsError> {
        let transfer = self.read_transfer()?;
        let bytes = transfer.encode()?;
        atomic_write(destination, &bytes)?;
        Ok(transfer)
    }

    /// Strictly validates a complete transfer, then atomically replaces this repository's file.
    ///
    /// Validation and encoding finish before any write. On malformed input,
    /// unsupported schemas or invalid values the existing file is left untouched.
    /// The envelope revision is restored verbatim; this replaces the whole
    /// settings document and never merges sections.
    pub fn import(&self, bytes: &[u8]) -> Result<SettingsTransfer, SettingsError> {
        let transfer = SettingsTransfer::decode(bytes)?;
        let canonical = transfer.encode()?;
        atomic_write(&self.path, &canonical)?;
        Ok(transfer)
    }

    fn read_transfer(&self) -> Result<SettingsTransfer, SettingsError> {
        let bytes = std::fs::read(&self.path).map_err(storage)?;
        Ok(SettingsTransfer::decode(&bytes)?)
    }
}
impl SettingsRepository for JsonSettingsRepository {
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        Ok(self.read_transfer()?.into_stored())
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        let transfer = SettingsTransfer::new(settings.clone())?;
        atomic_write(&self.path, &transfer.encode()?)?;
        Ok(())
    }
}

fn storage(error: std::io::Error) -> SettingsError {
    SettingsError::Storage(error.to_string())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let temporary = temporary_path(path)?;
    let result = write_and_replace(&temporary, path, bytes);
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(storage)
}

fn write_and_replace(temporary: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(temporary, path)
}

/// A unique sibling temporary file. A fresh name per write prevents one writer
/// from truncating another writer's or an orphaned temporary file.
fn temporary_path(path: &Path) -> Result<PathBuf, SettingsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    Ok(path.with_file_name(temporary))
}
