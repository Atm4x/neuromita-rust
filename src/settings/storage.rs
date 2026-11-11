use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use super::python_presets::sha256_hex;
use super::{
    AppSettings, PythonSettingsMigration, PythonSettingsPreview, SettingsError,
    SettingsImportFormat, SettingsInstanceLease, SettingsTransfer, SettingsTransferError,
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
    fn lifecycle_root(&self) -> Option<&Path> {
        None
    }
    fn json_path_for_owned_lifecycle(&self) -> Option<&Path> {
        None
    }
    fn has_application_root_owner(&self) -> bool {
        false
    }
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

/// Exact, value-free receipt for a standalone, stopped-app Python settings import.
/// A missing Rust target is distinct from any present file, including an empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonSettingsFileReview {
    pub source_sha256: String,
    pub target_settings_sha256: Option<String>,
    pub imported_keys: Vec<String>,
    pub skipped_keys: Vec<String>,
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
        let _maintenance = self.maintenance_lock()?;
        atomic_write(&self.path, &transfer.encode()?)?;
        Ok(transfer)
    }

    /// Offline-only snapshot while no application instance owns the data root.
    /// Does not grant permission to delete AI layers or modify running selections.
    pub fn inspect_for_maintenance(&self) -> Result<SettingsMaintenanceSnapshot, SettingsError> {
        let lease = self.maintenance_lock()?;
        let stored = self.load()?;
        Ok(SettingsMaintenanceSnapshot {
            stored,
            _lease: lease,
        })
    }

    pub(crate) fn save_with_owner(
        &self,
        settings: &StoredSettings,
        lease: &SettingsInstanceLease,
    ) -> Result<(), SettingsError> {
        if !lease
            .matches_root(self.path.parent().ok_or_else(|| {
                SettingsError::Storage("settings path has no portable root".into())
            })?)
            .map_err(|error| SettingsError::Storage(error.to_string()))?
        {
            return Err(SettingsError::Storage(
                "settings writer does not own the destination data root".into(),
            ));
        }
        let transfer = SettingsTransfer::new(settings.clone())?;
        atomic_write(&self.path, &transfer.encode()?)
    }

    fn maintenance_lock(&self) -> Result<SettingsInstanceLease, SettingsError> {
        let root = self
            .path
            .parent()
            .ok_or_else(|| SettingsError::Storage("settings path has no portable root".into()))?;
        SettingsInstanceLease::acquire(root)
            .map_err(|error| SettingsError::Storage(error.to_string()))
    }

    pub fn preview_python(&self, bytes: &[u8]) -> Result<PythonSettingsPreview, SettingsError> {
        let mut preview =
            PythonSettingsMigration.preview(bytes, &self.python_import_baseline()?)?;
        preview.changed |= !self.path.exists();
        Ok(preview)
    }

    /// Reads a single target snapshot under the same writer lock as application.
    /// Does not create or modify either settings document.
    pub fn review_python(
        &self,
        bytes: &[u8],
    ) -> Result<(PythonSettingsPreview, PythonSettingsFileReview), SettingsError> {
        let _lock = writer_lock(&self.path)?;
        self.python_review_snapshot(bytes)
    }

    fn python_review_snapshot(
        &self,
        bytes: &[u8],
    ) -> Result<(PythonSettingsPreview, PythonSettingsFileReview), SettingsError> {
        let original = if self.path.exists() {
            Some(std::fs::read(&self.path).map_err(|e| SettingsError::Storage(e.to_string()))?)
        } else {
            None
        };
        let baseline = match &original {
            Some(raw) => SettingsTransfer::decode(raw)?.into_stored(),
            None => StoredSettings {
                revision: 0,
                settings: AppSettings::initial_profile(),
            },
        };
        let mut preview = PythonSettingsMigration.preview(bytes, &baseline)?;
        preview.changed |= original.is_none();
        let review = PythonSettingsFileReview {
            source_sha256: sha256_hex(bytes),
            target_settings_sha256: original.as_deref().map(sha256_hex),
            imported_keys: preview.imported_keys.clone(),
            skipped_keys: preview.skipped_keys.clone(),
        };
        Ok((preview, review))
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
        if policy == PythonUnmappedPolicy::SkipAfterReview {
            return Err(SettingsError::Validation(
                "Python skipped-key approval requires an exact review receipt".into(),
            ));
        }
        self.import_python_internal(bytes, None)
    }

    /// Atomically revalidates the exact preview receipt before preparing a backup
    /// or changing the stopped profile. No one-bit permission can bypass it.
    pub fn import_python_reviewed(
        &self,
        bytes: &[u8],
        approved: &PythonSettingsFileReview,
    ) -> Result<PythonImportOutcome, SettingsError> {
        self.import_python_internal(bytes, Some(approved))
    }

    fn import_python_internal(
        &self,
        bytes: &[u8],
        approved: Option<&PythonSettingsFileReview>,
    ) -> Result<PythonImportOutcome, SettingsError> {
        let _instance = self.maintenance_lock()?;
        let _lock = writer_lock(&self.path)?;
        let (mut preview, actual) = self.python_review_snapshot(bytes)?;
        if let Some(approved) = approved {
            if actual != *approved {
                return Err(SettingsError::Validation(
                    "Python settings review no longer matches the source, target or skipped keys"
                        .into(),
                ));
            }
        } else if !preview.skipped_keys.is_empty() {
            return Err(SettingsTransferError::UnmappedPythonFields(preview.skipped_keys).into());
        }
        if !preview.changed {
            return Ok(PythonImportOutcome {
                preview,
                backup_path: None,
            });
        }
        let next_revision = preview
            .transfer
            .stored()
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
/// Complete persisted settings are available only while an exclusive offline
/// ownership lease is held. This does not authorize destructive layer changes.
pub struct SettingsMaintenanceSnapshot {
    stored: StoredSettings,
    _lease: SettingsInstanceLease,
}

impl SettingsMaintenanceSnapshot {
    pub fn stored(&self) -> &StoredSettings {
        &self.stored
    }
}

impl SettingsRepository for JsonSettingsRepository {
    fn lifecycle_root(&self) -> Option<&Path> {
        self.path.parent()
    }
    fn json_path_for_owned_lifecycle(&self) -> Option<&Path> {
        Some(&self.path)
    }
    fn migrate(&self) -> Result<(), SettingsError> {
        self.load().map(|_| ())
    }
    fn load(&self) -> Result<StoredSettings, SettingsError> {
        let bytes =
            std::fs::read(&self.path).map_err(|error| SettingsError::Storage(error.to_string()))?;
        Ok(SettingsTransfer::decode(&bytes)?.into_stored())
    }
    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        let _lease = self.maintenance_lock()?;
        let transfer = SettingsTransfer::new(settings.clone())?;
        atomic_write(&self.path, &transfer.encode()?)
    }
}

/// Application-only writer. Its explicit lease covers the entire operation and
/// cannot be recreated by a standalone JSON repository in another process.
pub(crate) struct LeasedJsonSettingsRepository {
    inner: JsonSettingsRepository,
    lease: Arc<SettingsInstanceLease>,
}

impl LeasedJsonSettingsRepository {
    pub(crate) fn new(path: PathBuf, lease: Arc<SettingsInstanceLease>) -> Self {
        Self {
            inner: JsonSettingsRepository::new(path),
            lease,
        }
    }
}

impl SettingsRepository for LeasedJsonSettingsRepository {
    fn lifecycle_root(&self) -> Option<&Path> {
        self.inner.lifecycle_root()
    }
    fn has_application_root_owner(&self) -> bool {
        true
    }

    fn migrate(&self) -> Result<(), SettingsError> {
        self.inner.migrate()
    }

    fn load(&self) -> Result<StoredSettings, SettingsError> {
        self.inner.load()
    }

    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        self.inner.save_with_owner(settings, &self.lease)
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

    #[test]
    fn direct_save_cannot_bypass_running_application_and_owned_writer_can_save() {
        let root = std::env::temp_dir().join(format!("nm-settings-write-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        let direct = JsonSettingsRepository::new(path.clone());
        let initial = StoredSettings {
            revision: 1,
            settings: AppSettings::initial_profile(),
        };
        direct.save(&initial).unwrap();
        let prior = std::fs::read(&path).unwrap();
        let lease = Arc::new(SettingsInstanceLease::acquire(&root).unwrap());
        let updated = StoredSettings {
            revision: 2,
            settings: AppSettings::initial_profile(),
        };
        assert!(direct.save(&updated).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), prior);
        let owned = LeasedJsonSettingsRepository::new(path.clone(), Arc::clone(&lease));
        owned.save(&updated).unwrap();
        assert_eq!(direct.load().unwrap(), updated);
        assert!(direct.save(&initial).is_err());
        drop(owned);
        drop(lease);
        direct.save(&initial).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_process_cannot_save_directly_until_root_owner_exits() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };

        let root =
            std::env::temp_dir().join(format!("nm-settings-writer-os-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let ready = root.join("owner-ready");
        let repository = JsonSettingsRepository::new(root.join("settings.json"));
        let initial = StoredSettings {
            revision: 1,
            settings: AppSettings::initial_profile(),
        };
        let later = StoredSettings {
            revision: 2,
            settings: AppSettings::initial_profile(),
        };
        repository.save(&initial).unwrap();
        let before = std::fs::read(root.join("settings.json")).unwrap();
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "settings::instance_lease::tests::subprocess_owner_fixture",
            ])
            .env("NM_SETTINGS_INSTANCE_CHILD_ROOT", &root)
            .env("NM_SETTINGS_INSTANCE_CHILD_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready.exists() {
            assert!(Instant::now() < deadline, "owner never acquired root lease");
            assert!(
                owner.try_wait().unwrap().is_none(),
                "owner exited before lock test"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(repository.save(&later).is_err());
        assert_eq!(std::fs::read(root.join("settings.json")).unwrap(), before);
        owner.kill().unwrap();
        owner.wait().unwrap();
        repository.save(&later).unwrap();
        assert_eq!(repository.load().unwrap(), later);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owned_writer_refuses_foreign_data_root() {
        let root =
            std::env::temp_dir().join(format!("nm-settings-other-root-{}", uuid::Uuid::new_v4()));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let lease = Arc::new(SettingsInstanceLease::acquire(&first).unwrap());
        let foreign = LeasedJsonSettingsRepository::new(second.join("settings.json"), lease);
        let stored = StoredSettings {
            revision: 1,
            settings: AppSettings::initial_profile(),
        };
        assert!(foreign.save(&stored).is_err());
        assert!(!second.join("settings.json").exists());
        drop(foreign);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn offline_import_and_maintenance_refuse_running_owner() {
        let root = std::env::temp_dir().join(format!("nm-settings-maint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let repository = JsonSettingsRepository::new(root.join("settings.json"));
        let persisted = StoredSettings {
            revision: 13,
            settings: AppSettings::initial_profile(),
        };
        repository.save(&persisted).unwrap();
        let lease = SettingsInstanceLease::acquire(&root).unwrap();
        let bytes = SettingsTransfer::new(persisted.clone())
            .unwrap()
            .encode()
            .unwrap();
        assert!(repository.inspect_for_maintenance().is_err());
        assert!(repository.import(&bytes).is_err());
        drop(lease);
        let snapshot = repository.inspect_for_maintenance().unwrap();
        assert_eq!(snapshot.stored(), &persisted);
        assert!(repository.import(&bytes).is_err());
        drop(snapshot);
        assert_eq!(repository.import(&bytes).unwrap().stored(), &persisted);
        std::fs::remove_dir_all(root).unwrap();
    }
}
