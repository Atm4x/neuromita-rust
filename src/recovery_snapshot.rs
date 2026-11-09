//! Explicit offline snapshot of the already-known local persistent files.
//! SessionArchive and all external owners are outside this contract.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    recovery_inventory::{self, InventoryError, RecoveryInventory},
    timeline::storage::SqliteTimelineRepository,
};

const MANIFEST_NAME: &str = "recovery-manifest.json";

struct Stage {
    path: PathBuf,
    published: bool,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl Stage {
    fn new(parent: &Path) -> Result<Self, InventoryError> {
        let path = parent.join(format!(".neuromita-recovery-{}.staging", Uuid::new_v4()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(&path)?;
            let metadata = fs::symlink_metadata(&path)?;
            Ok(Self {
                path,
                published: false,
                identity: (metadata.dev(), metadata.ino()),
            })
        }
        #[cfg(not(unix))]
        {
            fs::create_dir(&path)?;
            Ok(Self {
                path,
                published: false,
            })
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let Ok(metadata) = fs::symlink_metadata(&self.path) else {
                return;
            };
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || (metadata.dev(), metadata.ino()) != self.identity
            {
                return;
            }
        }
        for name in [
            "timeline.sqlite3",
            "timeline.sqlite3-wal",
            "timeline.sqlite3-shm",
            "settings.json",
            "presets.json",
            "preset-credentials.json",
            "finetune.json",
            MANIFEST_NAME,
        ] {
            let _ = fs::remove_file(self.path.join(name));
        }
        let _ = fs::remove_dir(&self.path);
    }
}

fn validate_path(path: &Path) -> Result<(), InventoryError> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(InventoryError::UnsafePath(path.display().to_string()));
    }
    Ok(())
}

fn validate_directory(path: &Path) -> Result<(), InventoryError> {
    validate_path(path)?;
    recovery_inventory::ensure_directory(path)
}

fn must_not_exist(path: &Path) -> Result<(), InventoryError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(InventoryError::UnsafePath(format!(
            "snapshot output already exists: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), InventoryError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        return File::open(path)?.sync_all();
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in staging path"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in destination path"))?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn rename_no_replace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-overwrite directory publication unsupported on this platform",
    ))
}

fn exact_snapshot_entries(root: &Path, manifest: &RecoveryInventory) -> Result<(), InventoryError> {
    let mut expected = BTreeSet::from(["timeline.sqlite3".to_owned(), MANIFEST_NAME.to_owned()]);
    for item in &manifest.config_files {
        expected.insert(item.path.clone());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            InventoryError::UnsafePath("snapshot contains a non-UTF-8 filename".into())
        })?;
        if !expected.remove(name) || !entry.file_type()?.is_file() {
            return Err(InventoryError::UnsafePath(format!(
                "unexpected snapshot entry: {name}"
            )));
        }
    }
    if !expected.is_empty() {
        return Err(InventoryError::Changed);
    }
    Ok(())
}

/// Verify a sealed known-files snapshot without modifying its contents.
/// The manifest is not signed, and the snapshot does not include SessionArchive.
pub fn verify(snapshot: impl AsRef<Path>) -> Result<RecoveryInventory, InventoryError> {
    let snapshot = snapshot.as_ref();
    validate_directory(snapshot)?;
    let manifest = recovery_inventory::read_manifest(snapshot.join(MANIFEST_NAME))?;
    exact_snapshot_entries(snapshot, &manifest)?;
    recovery_inventory::verify(snapshot, &manifest)
}

fn destination_parent_outside_source(
    source: &Path,
    destination: &Path,
) -> Result<PathBuf, InventoryError> {
    validate_directory(source)?;
    validate_path(destination)?;
    must_not_exist(destination)?;
    let parent = destination.parent().ok_or_else(|| {
        InventoryError::UnsafePath("snapshot output needs an existing directory".into())
    })?;
    validate_directory(parent)?;
    let original = fs::canonicalize(source)?;
    let parent_canonical = fs::canonicalize(parent)?;
    if parent_canonical.starts_with(&original) {
        return Err(InventoryError::UnsafePath(
            "destination must be outside the source directory".into(),
        ));
    }
    Ok(parent.to_path_buf())
}

fn publish_stage(
    stage: &mut Stage,
    destination: &Path,
    parent: &Path,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), InventoryError> {
    sync_directory(&stage.path)?;
    rename_no_replace(&stage.path, destination)?;
    stage.published = true;
    if let Err(error) = sync_parent(parent) {
        return Err(InventoryError::PublicationUncertain {
            destination: destination.to_path_buf(),
            cause: error.to_string(),
        });
    }
    Ok(())
}

fn copy_known_configs(
    source: &Path,
    target: &Path,
    inventory: &RecoveryInventory,
) -> Result<(), InventoryError> {
    for evidence in &inventory.config_files {
        let filename = Path::new(&evidence.path)
            .file_name()
            .filter(|name| recovery_inventory::CONFIG_FILES.contains(&name.to_str().unwrap_or("")))
            .ok_or_else(|| InventoryError::UnsafePath("unknown config file".into()))?;
        if filename != OsStr::new(&evidence.path) {
            return Err(InventoryError::UnsafePath("unsafe config path".into()));
        }
        let bytes = recovery_inventory::read_bounded_file(
            &source.join(filename),
            recovery_inventory::MAX_JSON_BYTES,
        )?;
        write_new(&target.join(filename), &bytes)?;
    }
    Ok(())
}

/// Restore a verified known-files snapshot into a newly published data directory.
/// The result intentionally omits the snapshot manifest: no source pathname, inode,
/// active configuration or other owner's files are changed or linked to the output.
/// This is not live cutover or complete dataset recovery (SessionArchive is excluded).
pub fn restore_into_new(
    snapshot: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<RecoveryInventory, InventoryError> {
    restore_with_sync(snapshot.as_ref(), destination.as_ref(), sync_directory)
}

fn restore_with_sync(
    snapshot: &Path,
    destination: &Path,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<RecoveryInventory, InventoryError> {
    restore_with_hooks(snapshot, destination, || Ok(()), sync_parent)
}

fn restore_with_hooks(
    snapshot: &Path,
    destination: &Path,
    after_stage_verified: impl FnOnce() -> Result<(), InventoryError>,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<RecoveryInventory, InventoryError> {
    let parent = destination_parent_outside_source(snapshot, destination)?;
    let expected = verify(snapshot)?;
    let mut stage = Stage::new(&parent)?;
    let restored = SqliteTimelineRepository::restore_verified_backup_into_new_database(
        snapshot.join("timeline.sqlite3"),
        stage.path.join("timeline.sqlite3"),
        &expected.timeline.fingerprint,
    )
    .map_err(|error| InventoryError::Timeline(error.to_string()))?;
    if restored.backup_fingerprint != expected.timeline.fingerprint
        || restored.revision != expected.timeline.revision
        || restored.canonical_events != expected.timeline.canonical_events
    {
        return Err(InventoryError::Changed);
    }
    copy_known_configs(snapshot, &stage.path, &expected)?;
    if recovery_inventory::inspect(&stage.path)? != expected {
        return Err(InventoryError::Changed);
    }
    after_stage_verified()?;
    if recovery_inventory::inspect(&stage.path)? != expected || verify(snapshot)? != expected {
        return Err(InventoryError::Changed);
    }
    publish_stage(&mut stage, destination, &parent, sync_parent)?;
    Ok(expected)
}

/// Create a new, atomically named directory with a verified SQLite snapshot and known JSONs.
/// Source must be stopped or externally excluded from concurrent writes; two inspections
/// do not constitute an application-wide consistency transaction.
pub fn create(
    root: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<RecoveryInventory, InventoryError> {
    create_with_sync(root.as_ref(), destination.as_ref(), sync_directory)
}

fn create_with_sync(
    root: &Path,
    destination: &Path,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<RecoveryInventory, InventoryError> {
    create_with_hooks(root, destination, || Ok(()), sync_parent)
}

fn create_with_hooks(
    root: &Path,
    destination: &Path,
    after_stage_verified: impl FnOnce() -> Result<(), InventoryError>,
    sync_parent: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<RecoveryInventory, InventoryError> {
    let parent = destination_parent_outside_source(root, destination)?;

    let before = recovery_inventory::inspect(root)?;
    let mut stage = Stage::new(&parent)?;
    let receipt = SqliteTimelineRepository::create_verified_backup(
        root.join("timeline.sqlite3"),
        stage.path.join("timeline.sqlite3"),
    )
    .map_err(|error| InventoryError::Timeline(error.to_string()))?;
    if receipt.source_fingerprint != before.timeline.fingerprint
        || receipt.revision != before.timeline.revision
        || receipt.canonical_events != before.timeline.canonical_events
    {
        return Err(InventoryError::Changed);
    }
    copy_known_configs(root, &stage.path, &before)?;
    if recovery_inventory::inspect(&stage.path)? != before {
        return Err(InventoryError::Changed);
    }
    write_new(
        &stage.path.join(MANIFEST_NAME),
        recovery_inventory::json(&before)?.as_bytes(),
    )?;
    if verify(&stage.path)? != before {
        return Err(InventoryError::Changed);
    }
    after_stage_verified()?;
    if verify(&stage.path)? != before || recovery_inventory::inspect(root)? != before {
        return Err(InventoryError::Changed);
    }
    publish_stage(&mut stage, destination, &parent, sync_parent)?;
    Ok(before)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::domain::{BranchId, SessionId, TimelineRepository};

    struct Fixture {
        parent: PathBuf,
        root: PathBuf,
        destination: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let parent =
                std::env::temp_dir().join(format!("neuromita-recovery-stage-{}", Uuid::new_v4()));
            fs::create_dir(&parent).unwrap();
            let root = parent.join("data");
            fs::create_dir(&root).unwrap();
            let mut repository =
                SqliteTimelineRepository::open(root.join("timeline.sqlite3")).unwrap();
            repository
                .create_session(SessionId::new(), BranchId::new())
                .unwrap();
            drop(repository);
            fs::write(root.join("settings.json"), "{\"secret\":true}").unwrap();
            let destination = parent.join("snapshot");
            Self {
                parent,
                root,
                destination,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }

    #[test]
    fn creates_verified_known_files_snapshot() {
        let fixture = Fixture::new();
        let receipt = create(&fixture.root, &fixture.destination).unwrap();
        assert_eq!(verify(&fixture.destination).unwrap(), receipt);
        assert_eq!(
            fs::read_to_string(fixture.destination.join("settings.json")).unwrap(),
            "{\"secret\":true}"
        );
        assert!(create(&fixture.root, &fixture.destination).is_err());
    }

    #[test]
    fn failed_prepublication_does_not_leave_destination() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("settings.json"), "bad-json").unwrap();
        assert!(create(&fixture.root, &fixture.destination).is_err());
        assert!(!fixture.destination.exists());
        assert!(fs::read_dir(&fixture.parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("staging")
        }));
    }

    #[test]
    fn competing_destination_created_during_staging_is_not_overwritten() {
        let fixture = Fixture::new();
        let result = create_with_hooks(
            &fixture.root,
            &fixture.destination,
            || {
                fs::create_dir(&fixture.destination)?;
                fs::write(fixture.destination.join("foreign"), "other owner")?;
                Ok(())
            },
            sync_directory,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(fixture.destination.join("foreign")).unwrap(),
            b"other owner"
        );
        assert!(fs::read_dir(&fixture.parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".neuromita-recovery-")
        }));
    }

    #[test]
    fn concurrent_source_mutation_refused_before_publication() {
        let fixture = Fixture::new();
        let result = create_with_hooks(
            &fixture.root,
            &fixture.destination,
            || {
                fs::write(fixture.root.join("settings.json"), "{\"secret\":false}")?;
                Ok(())
            },
            sync_directory,
        );
        assert!(matches!(result, Err(InventoryError::Changed)));
        assert!(!fixture.destination.exists());
    }

    #[test]
    fn restore_creates_independent_known_files_data_root() {
        let fixture = Fixture::new();
        create(&fixture.root, &fixture.destination).unwrap();
        let restored_root = fixture.parent.join("restored-root");
        let expected = verify(&fixture.destination).unwrap();
        assert_eq!(
            restore_into_new(&fixture.destination, &restored_root).unwrap(),
            expected
        );
        assert_eq!(
            recovery_inventory::inspect(&restored_root).unwrap(),
            expected
        );
        assert!(!restored_root.join(MANIFEST_NAME).exists());
        assert_eq!(
            fs::read(restored_root.join("settings.json")).unwrap(),
            b"{\"secret\":true}"
        );
        fs::write(restored_root.join("settings.json"), "{\"secret\":false}").unwrap();
        assert!(verify(&fixture.destination).is_ok());
        assert_eq!(
            fs::read(fixture.root.join("settings.json")).unwrap(),
            b"{\"secret\":true}"
        );
        assert!(restore_into_new(&fixture.destination, &restored_root).is_err());
    }

    #[test]
    fn restore_destination_race_keeps_foreign_files_and_cleans_private_stage() {
        let fixture = Fixture::new();
        create(&fixture.root, &fixture.destination).unwrap();
        let restored_root = fixture.parent.join("restored-root");
        let result = restore_with_hooks(
            &fixture.destination,
            &restored_root,
            || {
                fs::create_dir(&restored_root)?;
                fs::write(restored_root.join("foreign"), "another owner")?;
                Ok(())
            },
            sync_directory,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(restored_root.join("foreign")).unwrap(),
            b"another owner"
        );
        assert!(fs::read_dir(&fixture.parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".neuromita-recovery-")
        }));
        assert!(verify(&fixture.destination).is_ok());
    }

    #[test]
    fn restore_refuses_changed_source_after_private_staging() {
        let fixture = Fixture::new();
        create(&fixture.root, &fixture.destination).unwrap();
        let restored_root = fixture.parent.join("restored-root");
        let result = restore_with_hooks(
            &fixture.destination,
            &restored_root,
            || {
                fs::write(
                    fixture.destination.join("settings.json"),
                    "{\"secret\":false}",
                )?;
                Ok(())
            },
            sync_directory,
        );
        assert!(matches!(result, Err(InventoryError::Changed)));
        assert!(!restored_root.exists());
    }

    #[test]
    fn restore_postpublication_sync_failure_preserves_verified_data() {
        let fixture = Fixture::new();
        create(&fixture.root, &fixture.destination).unwrap();
        let restored_root = fixture.parent.join("restored-root");
        let result = restore_with_sync(&fixture.destination, &restored_root, |_| {
            Err(io::Error::other(
                "injected post-restore publication sync failure",
            ))
        });
        assert!(
            matches!(result, Err(InventoryError::PublicationUncertain { destination, .. }) if destination == restored_root)
        );
        assert_eq!(
            recovery_inventory::inspect(&restored_root).unwrap(),
            verify(&fixture.destination).unwrap()
        );
        assert!(!restored_root.join(MANIFEST_NAME).exists());
    }

    #[test]
    fn restore_rejects_inside_snapshot_before_any_mutation() {
        let fixture = Fixture::new();
        create(&fixture.root, &fixture.destination).unwrap();
        let child = fixture.destination.join("nested-data-root");
        assert!(restore_into_new(&fixture.destination, &child).is_err());
        assert!(!child.exists());
        assert!(verify(&fixture.destination).is_ok());
    }

    #[test]
    fn postpublication_sync_failure_preserves_snapshot() {
        let fixture = Fixture::new();
        let result = create_with_sync(&fixture.root, &fixture.destination, |_| {
            Err(io::Error::other("injected directory sync failure"))
        });
        assert!(matches!(
            result,
            Err(InventoryError::PublicationUncertain { .. })
        ));
        assert!(verify(&fixture.destination).is_ok());
    }
}
