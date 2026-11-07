use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{
    AppSettings, PythonSettingsMigration, PythonSettingsPreview, SettingsError,
    SettingsImportFormat, SettingsTransfer, SettingsTransferError,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSettings {
    pub revision: u64,
    pub settings: AppSettings,
}

pub trait SettingsRepository: Send + Sync + 'static {
    /// Validates the current document before publishing settings; does not migrate old formats.
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

/// Offline Python import outcome: a complete Rust transfer, preview and optional backup.
/// Source bytes are read-only; the backup is the previous Rust settings document.
#[derive(Debug)]
pub struct PythonImportOutcome {
    pub preview: PythonSettingsPreview,
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonUnmappedPolicy {
    Reject,
    SkipAfterReview,
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
        if format == SettingsImportFormat::Python {
            return Err(SettingsTransferError::PythonNeedsTarget.into());
        }
        let transfer = SettingsTransfer::prepare(format, bytes)?;
        atomic_write(&self.path, &transfer.encode()?)?;
        Ok(transfer)
    }

    pub fn preview_python(&self, bytes: &[u8]) -> Result<PythonSettingsPreview, SettingsError> {
        let mut preview =
            PythonSettingsMigration.preview(bytes, &self.python_import_baseline()?)?;
        preview.changed |= !self.path.exists();
        Ok(preview)
    }

    /// Intended only for a stopped application. Uses the settings writer lock
    /// for read/prepare/backup/publish; repeated imports are no-ops.
    pub fn import_python(&self, bytes: &[u8]) -> Result<PythonImportOutcome, SettingsError> {
        self.import_python_with_policy(bytes, PythonUnmappedPolicy::Reject)
    }

    pub fn import_python_with_policy(
        &self,
        bytes: &[u8],
        policy: PythonUnmappedPolicy,
    ) -> Result<PythonImportOutcome, SettingsError> {
        let _lock = writer_lock(&self.path)?;
        let baseline = self.python_import_baseline()?;
        let mut preview = PythonSettingsMigration.preview(bytes, &baseline)?;
        preview.changed |= !self.path.exists();
        if policy == PythonUnmappedPolicy::Reject && !preview.skipped_keys.is_empty() {
            return Err(SettingsTransferError::UnmappedPythonFields(preview.skipped_keys).into());
        }
        if !preview.changed {
            return Ok(PythonImportOutcome {
                preview,
                backup_path: None,
            });
        }
        let next_revision = baseline
            .revision
            .checked_add(1)
            .ok_or_else(|| SettingsError::Validation("settings revision exhausted".into()))?;
        let mut updated = preview.transfer.stored().clone();
        updated.revision = next_revision;
        let transfer = SettingsTransfer::new(updated)?;
        let encoded = transfer.encode()?;
        let backup_path = if self.path.exists() {
            Some(self.backup_for_python_import()?)
        } else {
            None
        };
        atomic_replace(&self.path, &encoded)?;
        preview.transfer = transfer;
        Ok(PythonImportOutcome {
            preview,
            backup_path,
        })
    }

    fn python_import_baseline(&self) -> Result<StoredSettings, SettingsError> {
        if self.path.exists() {
            self.load()
        } else {
            Ok(StoredSettings {
                revision: 0,
                settings: AppSettings::initial_profile(),
            })
        }
    }

    fn backup_for_python_import(&self) -> Result<PathBuf, SettingsError> {
        let original =
            std::fs::read(&self.path).map_err(|error| SettingsError::Storage(error.to_string()))?;
        let name = self
            .path
            .file_name()
            .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
        let mut name = name.to_os_string();
        name.push(format!(".pre-python-import-{}.bak", uuid::Uuid::new_v4()));
        let backup_path = self.path.with_file_name(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut backup = options
            .open(&backup_path)
            .map_err(|error| SettingsError::Storage(error.to_string()))?;
        let result = backup.write_all(&original).and_then(|_| backup.sync_all());
        if let Err(error) = result {
            drop(backup);
            let _ = std::fs::remove_file(&backup_path);
            return Err(SettingsError::Storage(error.to_string()));
        }
        Ok(backup_path)
    }
}
impl SettingsRepository for JsonSettingsRepository {
    fn migrate(&self) -> Result<(), SettingsError> {
        self.load().map(|_| ())
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

/// Serializes cooperating writers using a stable sidecar lock, then publishes
/// one complete file. Directory/power-loss durability is not promised.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let _lock = writer_lock(path)?;
    atomic_replace(path, bytes)
}

/// Explicitly releases the OS lock independently of inherited file handles.
pub(super) struct SettingsWriterLease {
    file: std::fs::File,
}

impl Drop for SettingsWriterLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(super) fn writer_lock(path: &Path) -> Result<SettingsWriterLease, SettingsError> {
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
    Ok(SettingsWriterLease { file })
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    let name = path
        .file_name()
        .ok_or_else(|| SettingsError::Storage("settings path has no file name".into()))?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let temporary = path.with_file_name(temporary);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
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
    #[cfg(unix)]
    #[test]
    fn writer_unlocks_while_inherited_descriptor_is_still_open() {
        let root = std::env::temp_dir().join(format!("settings-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        let first = writer_lock(&path).unwrap();
        let inherited = first.file.try_clone().unwrap();
        drop(first);
        let second = writer_lock(&path).unwrap();
        drop(second);
        drop(inherited);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_previous_dev_document_without_rewriting() {
        let path =
            std::env::temp_dir().join(format!("strict-settings-{}.json", uuid::Uuid::new_v4()));
        let original = serde_json::to_vec(&serde_json::json!({"schema_version":3,"revision":1,"settings":AppSettings::initial_profile()})).unwrap();
        std::fs::write(&path, &original).unwrap();
        let repo = JsonSettingsRepository::new(path.clone());
        assert!(repo.load().is_err());
        assert!(repo.migrate().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
    }
}
