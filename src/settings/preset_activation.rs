//! Offline, recoverable activation of a reviewed Python preset snapshot.
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use thiserror::Error;

use super::python_presets::{parse_credentials, parse_unique_json};
use super::{PresetId, PythonPresetStage, SettingsTransfer};
use crate::generation::StoredPresets;

const JOURNAL: &str = ".preset-activation-journal";
const STORAGE_LOCK: &str = ".preset-storage.lock";
const NAMES: [&str; 2] = ["presets.json", "preset-credentials.json"];
const SOURCES: [&str; 2] = [
    "existing-source-presets.json",
    "existing-source-preset-credentials.json",
];
const MAX_BYTES: [usize; 2] = [8 * 1024 * 1024, 1_048_576];
const FULL_NAMES: [&str; 3] = ["presets.json", "preset-credentials.json", "settings.json"];
const SETTINGS_LIMIT: usize = 8 * 1024 * 1024;
const SETTINGS_MARKER: &str = "includes-settings";

fn target_names(with_settings: bool) -> &'static [&'static str] {
    if with_settings { &FULL_NAMES } else { &NAMES }
}

fn target_limit(index: usize) -> usize {
    if index < MAX_BYTES.len() {
        MAX_BYTES[index]
    } else {
        SETTINGS_LIMIT
    }
}

#[derive(Debug, Error)]
pub enum PresetActivationError {
    #[error("unsafe or missing preset activation directory")]
    UnsafePath,
    #[error("preset activation requires an explicit recovery before starting the app")]
    RecoveryRequired,
    #[error("preset storage is currently locked by another application or operation")]
    StorageBusy,
    #[error("preset activation snapshot is invalid or no longer matches the stopped profile")]
    SnapshotConflict,
    #[error(
        "activation already committed but cleanup was interrupted; verify installed files before retry"
    )]
    CommittedNeedsCleanup,
    #[error("preset activation I/O: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetRecovery {
    NoJournal,
    RolledBack,
    Committed,
    DiscardedUnprepared,
}

/// An exclusive OS lease retained by the persistent catalog's repository.
/// All participants must use the same stable lock file; foreign applications
/// that ignore the lease are not prevented from editing files.
pub struct PresetStorageLease {
    _file: fs::File,
}

pub struct PresetActivation;

impl PresetActivation {
    /// Acquire an exclusive lease for the full lifetime of a persistent catalog.
    /// Holding the lease over catalog.open() and its subsequent writes ensures
    /// an offline activator cannot run concurrently with a cooperating Core.
    pub fn runtime_lease(active: &Path) -> Result<PresetStorageLease, PresetActivationError> {
        check_dir(active)?;
        let file = lock_storage(active)?;
        Self::ensure_ready(active)?;
        Ok(PresetStorageLease { _file: file })
    }

    /// Call at the application composition root BEFORE opening either preset file.
    pub fn ensure_ready(active: &Path) -> Result<(), PresetActivationError> {
        match fs::symlink_metadata(active.join(JOURNAL)) {
            Ok(_) => Err(PresetActivationError::RecoveryRequired),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PresetActivationError::Io(error)),
        }
    }

    /// Must only be called by an operator with both applications stopped.
    /// Never modifies the source review package, and never runs during startup.
    pub fn apply(review: &Path, active: &Path) -> Result<(), PresetActivationError> {
        Self::apply_inner(review, active, None, false)
    }

    /// Activate the reviewed Python user selection in the same recoverable
    /// transaction as presets and credentials. A valid existing settings.json
    /// is required; no default profile is synthesized.
    /// This operation is only for an explicitly stopped app.
    pub fn apply_with_preferences(
        review: &Path,
        active: &Path,
    ) -> Result<(), PresetActivationError> {
        Self::apply_inner(review, active, None, true)
    }

    fn apply_inner(
        review: &Path,
        active: &Path,
        fail_after_install: Option<usize>,
        with_settings: bool,
    ) -> Result<(), PresetActivationError> {
        check_dir(review)?;
        check_dir(active)?;
        let _lock = lock_storage(active)?;
        // Prevent cooperating settings CLI writers from racing the third
        // document while the preset lease protects the catalog pair.
        let _settings_lock = if with_settings {
            Some(
                super::storage::writer_lock(&active.join("settings.json"))
                    .map_err(|_| PresetActivationError::StorageBusy)?,
            )
        } else {
            None
        };
        Self::ensure_ready(active)?;
        if fs::canonicalize(review)? == fs::canonicalize(active)? {
            return Err(PresetActivationError::UnsafePath);
        }
        let mut new = Vec::with_capacity(2);
        let mut old = Vec::with_capacity(2);
        for i in 0..2 {
            let incoming = read_file(&review.join(NAMES[i]), MAX_BYTES[i])?
                .ok_or(PresetActivationError::SnapshotConflict)?;
            let previous = read_file(&active.join(NAMES[i]), MAX_BYTES[i])?;
            let expected = read_file(&review.join(SOURCES[i]), MAX_BYTES[i])?;
            if previous != expected {
                return Err(PresetActivationError::SnapshotConflict);
            }
            new.push(incoming);
            old.push(previous);
        }
        // Recreate the reviewed output from the source backups. A manually
        // edited or stale review package may not silently replace a profile.
        let python_presets =
            read_file(&review.join("python-source-api-presets.json"), MAX_BYTES[0])?
                .ok_or(PresetActivationError::SnapshotConflict)?;
        let python_templates = read_file(
            &review.join("python-source-api-templates.json"),
            MAX_BYTES[0],
        )?
        .ok_or(PresetActivationError::SnapshotConflict)?;
        let source_plan = PythonPresetStage::prepare(&python_presets, &python_templates)
            .map_err(|_| PresetActivationError::SnapshotConflict)?;
        let expected_plan = if old.iter().any(Option::is_some) {
            source_plan
                .merge_with_existing(old[0].as_deref(), old[1].as_deref())
                .map_err(|_| PresetActivationError::SnapshotConflict)?
        } else {
            source_plan
        };
        for (i, bytes) in new.iter().enumerate() {
            if expected_plan
                .file(NAMES[i])
                .map_err(|_| PresetActivationError::SnapshotConflict)?
                != bytes
            {
                return Err(PresetActivationError::SnapshotConflict);
            }
        }
        if with_settings {
            let python_settings =
                read_file(&review.join("python-source-settings.json"), SETTINGS_LIMIT)?
                    .ok_or(PresetActivationError::SnapshotConflict)?;
            let reviewed = read_file(
                &review.join("python-preset-preferences-review.json"),
                SETTINGS_LIMIT,
            )?
            .ok_or(PresetActivationError::SnapshotConflict)?;
            let expected_review = expected_plan
                .preview_preferences(&python_settings)
                .map_err(|_| PresetActivationError::SnapshotConflict)?;
            let expected_bytes = serde_json::to_vec_pretty(&expected_review)
                .map_err(|_| PresetActivationError::SnapshotConflict)?;
            if reviewed != expected_bytes || !expected_review.selection_explicit {
                return Err(PresetActivationError::SnapshotConflict);
            }
            let selected = expected_review
                .selected_user_id
                .ok_or(PresetActivationError::SnapshotConflict)?;
            let settings_original = read_file(&active.join("settings.json"), SETTINGS_LIMIT)?
                .ok_or(PresetActivationError::SnapshotConflict)?;
            let mut stored = SettingsTransfer::decode(&settings_original)
                .map_err(|_| PresetActivationError::SnapshotConflict)?
                .into_stored();
            stored.revision = stored
                .revision
                .checked_add(1)
                .ok_or(PresetActivationError::SnapshotConflict)?;
            stored.settings.presets.active = PresetId(selected);
            let encoded = SettingsTransfer::new(stored)
                .and_then(|value| value.encode())
                .map_err(|_| PresetActivationError::SnapshotConflict)?;
            if encoded.len() > SETTINGS_LIMIT {
                return Err(PresetActivationError::SnapshotConflict);
            }
            old.push(Some(settings_original));
            new.push(encoded);
        }
        validate_pair(&new[0], &new[1])?;
        if let Some(previous) = &old[0] {
            let credentials = old[1].as_deref().unwrap_or(b"{}");
            validate_pair(previous, credentials)?;
        } else if old[1].is_some() {
            parse_credentials(old[1].as_deref())
                .map_err(|_| PresetActivationError::SnapshotConflict)?;
        }
        let journal = active.join(JOURNAL);
        create_private_dir(&journal)?;
        let prepared = (|| -> Result<(), PresetActivationError> {
            for (i, bytes) in new.iter().enumerate() {
                write_private(&journal.join(format!("new-{i}")), bytes)?;
                if let Some(old_bytes) = &old[i] {
                    write_private(&journal.join(format!("old-{i}")), old_bytes)?;
                    write_private(&journal.join(format!("present-{i}")), b"")?;
                }
            }
            sync_dir(&journal)?;
            if with_settings {
                write_private(&journal.join(SETTINGS_MARKER), b"")?;
                sync_dir(&journal)?;
            }
            write_private(&journal.join("prepared"), b"")?;
            sync_dir(&journal)?;
            sync_dir(active)?;
            Ok(())
        })();
        if let Err(error) = prepared {
            // No active file was touched before the durable `prepared` marker.
            return Err(match Self::recover_locked(active) {
                Ok(_) => error,
                Err(_) => PresetActivationError::RecoveryRequired,
            });
        }
        crash_test_checkpoint("prepared");
        let result = (|| -> Result<(), PresetActivationError> {
            // Refuse stale snapshots before the first mutation. This is not a
            // replacement for ensuring the application has been shut down.
            for i in 0..old.len() {
                if read_file(
                    &active.join(target_names(with_settings)[i]),
                    target_limit(i),
                )? != old[i]
                {
                    return Err(PresetActivationError::SnapshotConflict);
                }
            }
            for i in 0..new.len() {
                install_bytes(&journal, active, target_names(with_settings)[i], &new[i])?;
                crash_test_checkpoint(match i {
                    0 => "installed-0",
                    1 => "installed-1",
                    _ => "installed-2",
                });
                if fail_after_install == Some(i) {
                    return Err(PresetActivationError::SnapshotConflict);
                }
            }
            sync_dir(active)?;
            write_private(&journal.join("committed"), b"")?;
            sync_dir(&journal)?;
            crash_test_checkpoint("committed");
            Ok(())
        })();
        if let Err(error) = result {
            return Err(match Self::recover_locked(active) {
                Ok(PresetRecovery::Committed) => PresetActivationError::CommittedNeedsCleanup,
                Ok(_) => error,
                Err(_) => PresetActivationError::RecoveryRequired,
            });
        }
        // The transaction is committed and durable; cleanup failure is not a rollback.
        finish_journal(active, &journal).map_err(|_| PresetActivationError::CommittedNeedsCleanup)
    }

    /// Explicit stopped-process recovery. A prepared but uncommitted transaction
    /// is rolled back; a committed transaction is left installed.
    pub fn recover(active: &Path) -> Result<PresetRecovery, PresetActivationError> {
        check_dir(active)?;
        let _lock = lock_storage(active)?;
        // Obtain the settings-writer lock whenever recovery could touch the
        // third document. Read is guarded by the already-held preset lease.
        let is_full = read_file(&active.join(JOURNAL).join(SETTINGS_MARKER), 0)?.is_some();
        let _settings_lock = if is_full {
            Some(
                super::storage::writer_lock(&active.join("settings.json"))
                    .map_err(|_| PresetActivationError::StorageBusy)?,
            )
        } else {
            None
        };
        Self::recover_locked(active)
    }

    fn recover_locked(active: &Path) -> Result<PresetRecovery, PresetActivationError> {
        let journal = active.join(JOURNAL);
        match fs::symlink_metadata(&journal) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PresetRecovery::NoJournal);
            }
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Err(PresetActivationError::UnsafePath),
        }
        let with_settings = read_file(&journal.join(SETTINGS_MARKER), 0)?.is_some();
        let names = target_names(with_settings);
        let committed = read_file(&journal.join("committed"), 0)?.is_some();
        let prepared = read_file(&journal.join("prepared"), 0)?.is_some();
        if committed {
            if !prepared {
                return Err(PresetActivationError::RecoveryRequired);
            }
            for (i, name) in names.iter().enumerate() {
                let new = read_file(&journal.join(format!("new-{i}")), target_limit(i))?
                    .ok_or(PresetActivationError::RecoveryRequired)?;
                if read_file(&active.join(name), target_limit(i))?.as_deref()
                    != Some(new.as_slice())
                {
                    return Err(PresetActivationError::RecoveryRequired);
                }
            }
        } else if prepared {
            let mut old = Vec::with_capacity(names.len());
            for i in 0..names.len() {
                let limit = target_limit(i);
                let snapshot = read_file(&journal.join(format!("old-{i}")), limit)?;
                let present = read_file(&journal.join(format!("present-{i}")), 0)?.is_some();
                if snapshot.is_some() != present {
                    return Err(PresetActivationError::RecoveryRequired);
                }
                let next = read_file(&journal.join(format!("new-{i}")), limit)?
                    .ok_or(PresetActivationError::RecoveryRequired)?;
                let current = read_file(&active.join(names[i]), limit)?;
                if let Some(current) = current.as_deref()
                    && Some(current) != snapshot.as_deref()
                    && current != next
                {
                    // An external writer changed the file after the crash.
                    // Refuse recovery rather than overwriting unrelated data.
                    return Err(PresetActivationError::SnapshotConflict);
                }
                old.push(snapshot);
            }
            for i in 0..names.len() {
                match &old[i] {
                    Some(bytes) => install_bytes(&journal, active, names[i], bytes)?,
                    None => remove_target(&journal, active, names[i])?,
                }
                if i == 0 {
                    crash_test_checkpoint("recovered-0");
                }
            }
            sync_dir(active)?;
        }
        let kind = if committed {
            PresetRecovery::Committed
        } else if prepared {
            PresetRecovery::RolledBack
        } else {
            PresetRecovery::DiscardedUnprepared
        };
        finish_journal(active, &journal)?;
        Ok(kind)
    }
}

fn validate_pair(catalog: &[u8], credentials: &[u8]) -> Result<(), PresetActivationError> {
    let parsed: StoredPresets = serde_json::from_value(
        parse_unique_json(catalog).map_err(|_| PresetActivationError::SnapshotConflict)?,
    )
    .map_err(|_| PresetActivationError::SnapshotConflict)?;
    parsed
        .validate_for_activation()
        .map_err(|_| PresetActivationError::SnapshotConflict)?;
    let secrets = parse_credentials(Some(credentials))
        .map_err(|_| PresetActivationError::SnapshotConflict)?;
    if parsed
        .local_credentials()
        .iter()
        .any(|name| !secrets.contains_key(*name))
    {
        return Err(PresetActivationError::SnapshotConflict);
    }
    Ok(())
}

fn lock_storage(active: &Path) -> Result<fs::File, PresetActivationError> {
    let path = active.join(STORAGE_LOCK);
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // The persistent file is deliberately never unlinked: replacing a lock
    // inode would allow independent processes to hold unrelated locks.
    let file = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            options.open(&path)?
        }
        Ok(_) => return Err(PresetActivationError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match options.create_new(true).open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    options.create_new(false);
                    options.open(&path)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    let named = fs::symlink_metadata(&path)?;
    if !named.is_file() || named.file_type().is_symlink() || !file.metadata()?.is_file() {
        return Err(PresetActivationError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if opened.dev() != named.dev() || opened.ino() != named.ino() {
            return Err(PresetActivationError::UnsafePath);
        }
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(PresetActivationError::StorageBusy),
        Err(std::fs::TryLockError::Error(error)) => Err(PresetActivationError::Io(error)),
    }
}

fn check_dir(path: &Path) -> Result<(), PresetActivationError> {
    if !path.is_absolute() {
        return Err(PresetActivationError::UnsafePath);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| PresetActivationError::UnsafePath)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PresetActivationError::UnsafePath);
    }
    Ok(())
}

fn read_file(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, PresetActivationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(PresetActivationError::UnsafePath);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        _ => {}
    }
    if fs::metadata(path)?.len() > limit as u64 {
        return Err(PresetActivationError::SnapshotConflict);
    }
    let bytes = fs::read(path)?;
    if bytes.len() > limit {
        return Err(PresetActivationError::SnapshotConflict);
    }
    Ok(Some(bytes))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

fn install_bytes(
    journal: &Path,
    active: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), PresetActivationError> {
    let temp = journal.join(format!("install-{}", uuid::Uuid::new_v4().simple()));
    write_private(&temp, bytes)?;
    remove_target(journal, active, name)?;
    fs::rename(&temp, active.join(name))?;
    Ok(())
}

fn remove_target(journal: &Path, active: &Path, name: &str) -> Result<(), PresetActivationError> {
    let target = active.join(name);
    let limit = match name {
        "presets.json" => MAX_BYTES[0],
        "preset-credentials.json" => MAX_BYTES[1],
        "settings.json" => SETTINGS_LIMIT,
        _ => return Err(PresetActivationError::UnsafePath),
    };
    if read_file(&target, limit)?.is_some() {
        fs::rename(
            target,
            journal.join(format!("discard-{}", uuid::Uuid::new_v4().simple())),
        )?;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn finish_journal(active: &Path, journal: &Path) -> Result<(), PresetActivationError> {
    let finished = active.join(format!(
        ".preset-activation-finished-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::rename(journal, &finished)?;
    sync_dir(active)?;
    fs::remove_dir_all(&finished)?;
    sync_dir(active)?;
    Ok(())
}

// Crash points are compiled into unit tests only. Production builds have no
// environment-controlled termination path.
#[cfg(test)]
fn crash_test_checkpoint(point: &str) {
    if std::env::var("NEUROMITA_PRESET_TEST_ABORT_AT").as_deref() == Ok(point) {
        std::process::abort();
    }
}

#[cfg(not(test))]
#[inline(always)]
fn crash_test_checkpoint(_: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(std::path::PathBuf);
    impl Root {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("preset-tx-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn runtime_lease_excludes_second_core_and_offline_operators() {
        let root = Root::new();
        let first = PresetActivation::runtime_lease(root.path()).unwrap();
        assert!(matches!(
            PresetActivation::runtime_lease(root.path()),
            Err(PresetActivationError::StorageBusy)
        ));
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::StorageBusy)
        ));
        assert_eq!(
            fs::metadata(root.path().join(STORAGE_LOCK)).unwrap().len(),
            0
        );
        drop(first);
        let second = PresetActivation::runtime_lease(root.path()).unwrap();
        drop(second);
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::NoJournal
        );
        assert!(root.path().join(STORAGE_LOCK).is_file());
    }

    #[test]
    fn exclusive_operator_lease_rejects_core_start_and_other_recovery() {
        let root = Root::new();
        let exclusive = lock_storage(root.path()).unwrap();
        assert!(matches!(
            PresetActivation::runtime_lease(root.path()),
            Err(PresetActivationError::StorageBusy)
        ));
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::StorageBusy)
        ));
        drop(exclusive);
        let lease = PresetActivation::runtime_lease(root.path()).unwrap();
        drop(lease);
    }

    #[test]
    fn lock_is_cross_process_not_just_an_in_process_mutex() {
        let root = Root::new();
        let exclusive = lock_storage(root.path()).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("settings::preset_activation::tests::preset_lease_child_fixture")
            .arg("--nocapture")
            .env("NEUROMITA_PRESET_LOCK_TEST_ROOT", root.path())
            .status()
            .unwrap();
        assert!(status.success());
        drop(exclusive);
    }

    #[test]
    fn preset_lease_child_fixture() {
        if let Some(root) = std::env::var_os("NEUROMITA_PRESET_LOCK_TEST_ROOT") {
            assert!(matches!(
                PresetActivation::runtime_lease(Path::new(&root)),
                Err(PresetActivationError::StorageBusy)
            ));
        }
    }

    #[test]
    fn runtime_lease_enforces_pending_journal_guard_and_recovery_still_works() {
        let root = Root::new();
        create_private_dir(&root.path().join(JOURNAL)).unwrap();
        assert!(matches!(
            PresetActivation::runtime_lease(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::DiscardedUnprepared
        );
        let lease = PresetActivation::runtime_lease(root.path()).unwrap();
        drop(lease);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_lock_file_is_rejected_without_following_target() {
        use std::os::unix::fs::symlink;
        let root = Root::new();
        let external = root.path().join("untouched");
        write_private(&external, b"unrelated").unwrap();
        symlink(&external, root.path().join(STORAGE_LOCK)).unwrap();
        assert!(matches!(
            PresetActivation::runtime_lease(root.path()),
            Err(PresetActivationError::UnsafePath)
        ));
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::UnsafePath)
        ));
        assert_eq!(fs::read(external).unwrap(), b"unrelated");
    }

    #[test]
    fn incomplete_prepared_journal_is_discarded_without_touching_targets() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        write_private(&journal.join("new-0"), b"private").unwrap();
        assert!(matches!(
            PresetActivation::ensure_ready(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::DiscardedUnprepared
        );
        assert!(!journal.exists());
    }

    #[test]
    fn interruption_between_two_file_installs_restores_exact_old_bytes() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        let old = [b"old-records".as_slice(), b"old-keys".as_slice()];
        let next = [b"new-records".as_slice(), b"new-keys".as_slice()];
        for (i, name) in NAMES.iter().enumerate() {
            write_private(&root.path().join(name), old[i]).unwrap();
            write_private(&journal.join(format!("old-{i}")), old[i]).unwrap();
            write_private(&journal.join(format!("present-{i}")), b"").unwrap();
            write_private(&journal.join(format!("new-{i}")), next[i]).unwrap();
        }
        write_private(&journal.join("prepared"), b"").unwrap();
        install_bytes(&journal, root.path(), NAMES[0], next[0]).unwrap();
        assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), next[0]);
        assert!(matches!(
            PresetActivation::ensure_ready(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::RolledBack
        );
        for (i, name) in NAMES.iter().enumerate() {
            assert_eq!(fs::read(root.path().join(name)).unwrap(), old[i]);
        }
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::NoJournal
        );
    }

    #[test]
    fn committed_transaction_is_never_rolled_back_and_can_be_cleaned() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for (i, name) in NAMES.iter().enumerate() {
            let bytes = format!("new-{i}");
            write_private(&root.path().join(name), bytes.as_bytes()).unwrap();
            write_private(&journal.join(format!("new-{i}")), bytes.as_bytes()).unwrap();
        }
        write_private(&journal.join("prepared"), b"").unwrap();
        write_private(&journal.join("committed"), b"").unwrap();
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::Committed
        );
        assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), b"new-0");
        assert!(!journal.exists());
    }

    #[test]
    fn failed_first_install_rolls_back_and_retry_can_commit() {
        let root = Root::new();
        let active = root.path().join("UserData");
        fs::create_dir(&active).unwrap();
        let review = root.path().join("review");
        let templates = serde_json::json!({"templates":{"3":{"name":"Google","protocol_id":"google_gemini_default","url":"","default_model":"gemini-2.5-flash"}}});
        let presets = serde_json::json!({"presets":{"18":{
            "id":18,"name":"Offline test","base":3,"protocol_id":"","protocol_overrides":{},
            "url":"","default_model":"gemini-2.5-flash","key":"fixture-token",
            "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},
            "enabled":["max_tokens","thinking_level"]}
        }}});
        let prepared = PythonPresetStage::prepare(
            &serde_json::to_vec(&presets).unwrap(),
            &serde_json::to_vec(&templates).unwrap(),
        )
        .unwrap();
        prepared.stage_new_directory(&review).unwrap();
        assert!(matches!(
            PresetActivation::apply_inner(&review, &active, Some(0), false),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert!(!active.join(NAMES[0]).exists());
        assert!(!active.join(NAMES[1]).exists());
        assert!(!active.join(JOURNAL).exists());
        PresetActivation::apply(&review, &active).unwrap();
        assert_eq!(
            fs::read(active.join(NAMES[0])).unwrap(),
            fs::read(review.join(NAMES[0])).unwrap()
        );
        assert_eq!(
            fs::read(active.join(NAMES[1])).unwrap(),
            fs::read(review.join(NAMES[1])).unwrap()
        );
    }

    #[test]
    fn missing_old_backup_is_never_interpreted_as_original_absence() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for i in 0..2 {
            write_private(&journal.join(format!("new-{i}")), b"replacement").unwrap();
        }
        write_private(&journal.join("present-0"), b"").unwrap();
        write_private(&journal.join("prepared"), b"").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert!(journal.exists());
    }

    #[test]
    fn a_committed_but_mismatched_snapshot_requires_operator_repair() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        write_private(&journal.join("committed"), b"").unwrap();
        write_private(&journal.join("new-0"), b"expected").unwrap();
        write_private(&journal.join("new-1"), b"secret").unwrap();
        write_private(&root.path().join(NAMES[0]), b"mismatch").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert!(journal.exists());
    }

    fn fixture_input(id: u32) -> (Vec<u8>, Vec<u8>) {
        let templates = serde_json::json!({"templates":{"3":{
            "name":"Google", "protocol_id":"google_gemini_default",
            "url":"", "default_model":"gemini-2.5-flash"
        }}});
        let presets = serde_json::json!({"presets":{id.to_string():{
            "id":id, "name":format!("Preset {id}"), "base":3,
            "protocol_id":"", "protocol_overrides":{}, "url":"",
            "default_model":"gemini-2.5-flash", "key":format!("private-{id}"),
            "model_settings":{"values":{
                "temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"
            },"enabled":["max_tokens","thinking_level"]}
        }}});
        (
            serde_json::to_vec(&presets).unwrap(),
            serde_json::to_vec(&templates).unwrap(),
        )
    }

    #[cfg(unix)]
    fn crash_fixture(
        root: &Root,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        [Vec<u8>; 2],
        [Vec<u8>; 2],
    ) {
        let active = root.path().join("UserData");
        fs::create_dir(&active).unwrap();
        let (old_presets, old_templates) = fixture_input(7);
        let old_stage = PythonPresetStage::prepare(&old_presets, &old_templates).unwrap();
        let old = [
            old_stage.file(NAMES[0]).unwrap().to_vec(),
            old_stage.file(NAMES[1]).unwrap().to_vec(),
        ];
        for (name, bytes) in NAMES.iter().zip(old.iter()) {
            write_private(&active.join(name), bytes).unwrap();
        }
        let (incoming, templates) = fixture_input(18);
        let next_stage = PythonPresetStage::prepare(&incoming, &templates)
            .unwrap()
            .merge_with_existing(Some(&old[0]), Some(&old[1]))
            .unwrap();
        let next = [
            next_stage.file(NAMES[0]).unwrap().to_vec(),
            next_stage.file(NAMES[1]).unwrap().to_vec(),
        ];
        let review = root.path().join("review с пробелами");
        next_stage.stage_new_directory(&review).unwrap();
        (review, active, old, next)
    }

    #[cfg(unix)]
    fn three_file_crash_fixture(
        root: &Root,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        [Vec<u8>; 3],
        [Vec<u8>; 3],
    ) {
        let (old_review, active, old_pair, _) = crash_fixture(root);
        let old_settings = SettingsTransfer::new(crate::settings::StoredSettings {
            revision: 6,
            settings: crate::settings::AppSettings::initial_profile(),
        })
        .unwrap()
        .encode()
        .unwrap();
        write_private(&active.join("settings.json"), &old_settings).unwrap();
        fs::remove_dir_all(old_review).unwrap();
        let (incoming, templates) = fixture_input(18);
        let plan = PythonPresetStage::prepare(&incoming, &templates)
            .unwrap()
            .merge_with_existing(Some(&old_pair[0]), Some(&old_pair[1]))
            .unwrap();
        let (plan, _) = plan
            .with_preferences(br#"{"LAST_API_PRESET_ID":18}"#)
            .unwrap();
        let review = root.path().join("three-files");
        plan.stage_new_directory(&review).unwrap();
        let mut updated = SettingsTransfer::decode(&old_settings)
            .unwrap()
            .into_stored();
        updated.revision += 1;
        updated.settings.presets.active = PresetId(18);
        let new_settings = SettingsTransfer::new(updated).unwrap().encode().unwrap();
        let old = [old_pair[0].clone(), old_pair[1].clone(), old_settings];
        let next = [
            plan.file(NAMES[0]).unwrap().to_vec(),
            plan.file(NAMES[1]).unwrap().to_vec(),
            new_settings,
        ];
        (review, active, old, next)
    }

    #[cfg(unix)]
    #[test]
    fn real_three_file_crashes_recover_every_boundary() {
        for marker in [
            "prepared",
            "installed-0",
            "installed-1",
            "installed-2",
            "committed",
        ] {
            let root = Root::new();
            let (review, active, old, next) = three_file_crash_fixture(&root);
            use std::os::unix::process::ExitStatusExt;
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("settings::preset_activation::tests::abort_child_runner")
                .env("NEUROMITA_PRESET_TEST_ABORT_AT", marker)
                .env("NEUROMITA_TEST_THREE_FILES", "1")
                .env("NEUROMITA_TEST_REVIEW", &review)
                .env("NEUROMITA_TEST_ACTIVE", &active)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.signal(), Some(6), "{marker}: {status}");
            assert!(matches!(
                PresetActivation::ensure_ready(&active),
                Err(PresetActivationError::RecoveryRequired)
            ));
            let recovered = PresetActivation::recover(&active).unwrap();
            let expected = if marker == "committed" {
                assert_eq!(recovered, PresetRecovery::Committed);
                &next
            } else {
                assert_eq!(recovered, PresetRecovery::RolledBack);
                &old
            };
            for (name, bytes) in FULL_NAMES.iter().zip(expected.iter()) {
                assert_eq!(
                    fs::read(active.join(name)).unwrap(),
                    *bytes,
                    "{marker} {name}"
                );
            }
            assert_eq!(
                PresetActivation::recover(&active).unwrap(),
                PresetRecovery::NoJournal
            );
        }
    }

    #[test]
    fn three_file_recovery_refuses_foreign_settings_edit_without_touching_presets() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for (i, name) in FULL_NAMES.iter().enumerate() {
            let old = format!("old-{i}");
            let new = format!("new-{i}");
            write_private(&root.path().join(name), new.as_bytes()).unwrap();
            write_private(&journal.join(format!("old-{i}")), old.as_bytes()).unwrap();
            write_private(&journal.join(format!("new-{i}")), new.as_bytes()).unwrap();
            write_private(&journal.join(format!("present-{i}")), b"").unwrap();
        }
        write_private(&journal.join(SETTINGS_MARKER), b"").unwrap();
        write_private(&journal.join("prepared"), b"").unwrap();
        fs::write(root.path().join("settings.json"), b"foreign settings write").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(
            fs::read(root.path().join("presets.json")).unwrap(),
            b"new-0"
        );
        assert_eq!(
            fs::read(root.path().join("preset-credentials.json")).unwrap(),
            b"new-1"
        );
        assert_eq!(
            fs::read(root.path().join("settings.json")).unwrap(),
            b"foreign settings write"
        );
        assert!(journal.is_dir());
    }

    #[test]
    fn incomplete_three_file_journal_requires_manual_inspection() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for i in 0..2 {
            write_private(&journal.join(format!("old-{i}")), b"old").unwrap();
            write_private(&journal.join(format!("present-{i}")), b"").unwrap();
            write_private(&journal.join(format!("new-{i}")), b"new").unwrap();
        }
        write_private(&journal.join(SETTINGS_MARKER), b"").unwrap();
        write_private(&journal.join("prepared"), b"").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert!(journal.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn abort_child_runner() {
        let Ok(marker) = std::env::var("NEUROMITA_PRESET_TEST_ABORT_AT") else {
            return;
        };
        let active = std::path::PathBuf::from(std::env::var_os("NEUROMITA_TEST_ACTIVE").unwrap());
        if marker == "recovered-0" {
            PresetActivation::recover(&active).unwrap();
        } else {
            let review =
                std::path::PathBuf::from(std::env::var_os("NEUROMITA_TEST_REVIEW").unwrap());
            if std::env::var_os("NEUROMITA_TEST_THREE_FILES").is_some() {
                PresetActivation::apply_with_preferences(&review, &active).unwrap();
            } else {
                PresetActivation::apply(&review, &active).unwrap();
            }
        }
        panic!("crash injection point {marker} was not reached");
    }

    #[cfg(unix)]
    fn abort_process_at(marker: &str, review: &Path, active: &Path) {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("settings::preset_activation::tests::abort_child_runner")
            .arg("--nocapture")
            .env("NEUROMITA_PRESET_TEST_ABORT_AT", marker)
            .env("NEUROMITA_TEST_REVIEW", review)
            .env("NEUROMITA_TEST_ACTIVE", active)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(6),
            "{marker}: child did not abort at checkpoint: {status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn actual_subprocess_crashes_recover_both_files_at_every_commit_boundary() {
        for marker in ["prepared", "installed-0", "installed-1", "committed"] {
            let root = Root::new();
            let (review, active, old, next) = crash_fixture(&root);
            abort_process_at(marker, &review, &active);
            assert!(matches!(
                PresetActivation::ensure_ready(&active),
                Err(PresetActivationError::RecoveryRequired)
            ));
            let result = PresetActivation::recover(&active).unwrap();
            let expected = if marker == "committed" {
                assert_eq!(result, PresetRecovery::Committed);
                &next
            } else {
                assert_eq!(result, PresetRecovery::RolledBack);
                &old
            };
            for (name, bytes) in NAMES.iter().zip(expected.iter()) {
                assert_eq!(
                    fs::read(active.join(name)).unwrap(),
                    *bytes,
                    "{marker} {name}"
                );
            }
            assert_eq!(
                PresetActivation::recover(&active).unwrap(),
                PresetRecovery::NoJournal
            );
            PresetActivation::ensure_ready(&active).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn recovery_is_idempotent_after_a_second_real_process_crash() {
        let root = Root::new();
        let (review, active, old, _) = crash_fixture(&root);
        abort_process_at("installed-0", &review, &active);
        abort_process_at("recovered-0", &review, &active);
        assert!(matches!(
            PresetActivation::ensure_ready(&active),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(&active).unwrap(),
            PresetRecovery::RolledBack
        );
        for (name, bytes) in NAMES.iter().zip(old.iter()) {
            assert_eq!(fs::read(active.join(name)).unwrap(), *bytes);
        }
        assert_eq!(
            PresetActivation::recover(&active).unwrap(),
            PresetRecovery::NoJournal
        );
    }

    #[test]
    fn recovery_refuses_external_mutation_before_replacing_either_file() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for (i, name) in NAMES.iter().enumerate() {
            write_private(
                &journal.join(format!("old-{i}")),
                format!("old-{i}").as_bytes(),
            )
            .unwrap();
            write_private(
                &journal.join(format!("new-{i}")),
                format!("new-{i}").as_bytes(),
            )
            .unwrap();
            write_private(&journal.join(format!("present-{i}")), b"").unwrap();
            write_private(&root.path().join(name), format!("new-{i}").as_bytes()).unwrap();
        }
        write_private(&journal.join("prepared"), b"").unwrap();
        fs::write(root.path().join(NAMES[1]), b"outside writer data").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), b"new-0");
        assert_eq!(
            fs::read(root.path().join(NAMES[1])).unwrap(),
            b"outside writer data"
        );
        assert!(journal.exists());
    }
}
