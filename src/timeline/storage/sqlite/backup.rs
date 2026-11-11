use std::{
    fs, io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use super::{Connection, SqliteTimelineRepository, storage};
use crate::timeline::domain::TimelineError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedTimelineBackup {
    pub revision: u64,
    pub canonical_events: usize,
    pub source_fingerprint: String,
}

/// Verification receipt for restoring a complete Timeline SQLite file into a new location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedTimelineRestore {
    pub revision: u64,
    pub canonical_events: usize,
    pub backup_fingerprint: String,
}

struct StagedBackup(PathBuf);

#[cfg(unix)]
#[derive(Clone, Copy)]
struct SourceIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl SourceIdentity {
    fn capture(path: &Path) -> Result<Self, TimelineError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path).map_err(io_error)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn verify(&self, path: &Path) -> Result<(), TimelineError> {
        let current = Self::capture(path)?;
        if current.device != self.device || current.inode != self.inode {
            return Err(TimelineError::ConcurrentWrite);
        }
        Ok(())
    }
}

impl Drop for StagedBackup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        for suffix in ["-wal", "-shm"] {
            let mut path = self.0.as_os_str().to_os_string();
            path.push(suffix);
            let _ = fs::remove_file(PathBuf::from(path));
        }
    }
}

fn io_error(error: io::Error) -> TimelineError {
    storage(error.to_string())
}

fn verify_sqlite_integrity(connection: &Connection) -> Result<(), TimelineError> {
    if connection.query("PRAGMA integrity_check(1)", &[])? != [vec!["ok".to_owned()]] {
        return Err(storage("SQLite integrity check failed"));
    }
    if !connection
        .query("SELECT * FROM pragma_foreign_key_check LIMIT 1", &[])?
        .is_empty()
    {
        return Err(storage("SQLite foreign key check failed"));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory).and_then(|file| file.sync_all())
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    Ok(())
}

impl SqliteTimelineRepository {
    /// Inspect a pre-existing Timeline SQLite snapshot without canonical writes or repair.
    /// Validates SQLite integrity, foreign keys, canonical/derived indexes and a stable
    /// optimistic fingerprint. Does not establish provenance or exclusive writer ownership.
    pub fn inspect_verified_snapshot(
        source: impl AsRef<Path>,
    ) -> Result<VerifiedTimelineBackup, TimelineError> {
        let source = source.as_ref();
        if !source.is_absolute() || !source.is_file() {
            return Err(storage(
                "snapshot source must be an existing absolute SQLite file",
            ));
        }
        #[cfg(unix)]
        let identity = SourceIdentity::capture(source)?;
        let preview = Self::inspect_derived_indexes(source)?;
        if preview.requires_repair() {
            return Err(storage(
                "source derived indexes need repair before inspection",
            ));
        }
        let read_only = Connection::open_read_only(source)?;
        verify_sqlite_integrity(&read_only)?;
        drop(read_only);
        let final_preview = Self::inspect_derived_indexes(source)?;
        if final_preview.requires_repair() || final_preview.confirmation != preview.confirmation {
            return Err(TimelineError::ConcurrentWrite);
        }
        #[cfg(unix)]
        identity.verify(source)?;
        Ok(VerifiedTimelineBackup {
            revision: preview.revision,
            canonical_events: preview.canonical_events,
            source_fingerprint: preview.confirmation,
        })
    }

    /// Create a new, stand-alone SQLite snapshot containing the complete Timeline database.
    /// Includes committed WAL frames, validates the original and backup, and never overwrites
    /// an existing destination. Requires an offline, trusted operator; this is not a live
    /// application lock or a backup of files owned by other components.
    pub fn create_verified_backup(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<VerifiedTimelineBackup, TimelineError> {
        Self::publish_verified_snapshot(source.as_ref(), destination.as_ref(), None)
    }

    /// Restore a previously verified Timeline backup into a new, non-existing SQLite path.
    /// The expected fingerprint comes from the backup receipt and is checked before and
    /// after snapshot creation. This cannot overwrite or switch an active application DB.
    pub fn restore_verified_backup_into_new_database(
        backup: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        expected_fingerprint: &str,
    ) -> Result<VerifiedTimelineRestore, TimelineError> {
        if expected_fingerprint.len() != 64
            || !expected_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TimelineError::Invalid(
                "invalid Timeline backup fingerprint",
            ));
        }
        let normalized_fingerprint = expected_fingerprint.to_ascii_lowercase();
        let receipt = Self::publish_verified_snapshot(
            backup.as_ref(),
            destination.as_ref(),
            Some(&normalized_fingerprint),
        )?;
        Ok(VerifiedTimelineRestore {
            revision: receipt.revision,
            canonical_events: receipt.canonical_events,
            backup_fingerprint: receipt.source_fingerprint,
        })
    }

    fn publish_verified_snapshot(
        source: &Path,
        destination: &Path,
        expected_fingerprint: Option<&str>,
    ) -> Result<VerifiedTimelineBackup, TimelineError> {
        Self::publish_verified_snapshot_with(source, destination, expected_fingerprint, || Ok(()))
    }

    fn publish_verified_snapshot_with(
        source: &Path,
        destination: &Path,
        expected_fingerprint: Option<&str>,
        after_stage_verified: impl FnOnce() -> Result<(), TimelineError>,
    ) -> Result<VerifiedTimelineBackup, TimelineError> {
        Self::publish_verified_snapshot_with_sync(
            source,
            destination,
            expected_fingerprint,
            after_stage_verified,
            |parent, _destination| sync_directory(parent),
        )
    }

    fn publish_verified_snapshot_with_sync(
        source: &Path,
        destination: &Path,
        expected_fingerprint: Option<&str>,
        after_stage_verified: impl FnOnce() -> Result<(), TimelineError>,
        sync_parent: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<VerifiedTimelineBackup, TimelineError> {
        if !source.is_absolute() || !source.is_file() {
            return Err(storage(
                "backup source must be an existing absolute SQLite file",
            ));
        }
        if !destination.is_absolute() {
            return Err(storage("backup destination must be an absolute path"));
        }
        match fs::symlink_metadata(destination) {
            Ok(_) => return Err(storage("backup destination already exists")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        let parent = destination
            .parent()
            .filter(|parent| parent.is_dir())
            .ok_or_else(|| storage("backup destination directory must already exist"))?;
        #[cfg(unix)]
        let source_identity = SourceIdentity::capture(source)?;
        let source_preview = Self::inspect_derived_indexes(source)?;
        if source_preview.requires_repair() {
            return Err(storage("source derived indexes need repair before backup"));
        }
        if expected_fingerprint.is_some_and(|expected| expected != source_preview.confirmation) {
            return Err(TimelineError::ConcurrentWrite);
        }
        let read_only = Connection::open_read_only(source)?;
        verify_sqlite_integrity(&read_only)?;

        let staged =
            StagedBackup(parent.join(format!(".neuromita-timeline-backup-{}.tmp", Uuid::new_v4())));
        read_only.vacuum_into(&staged.0)?;
        drop(read_only);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged.0, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
        }

        let staged_connection = Connection::open_read_only(&staged.0)?;
        verify_sqlite_integrity(&staged_connection)?;
        drop(staged_connection);
        let staged_preview = Self::inspect_derived_indexes(&staged.0)?;
        if staged_preview.requires_repair()
            || staged_preview.confirmation != source_preview.confirmation
        {
            return Err(TimelineError::ConcurrentWrite);
        }
        after_stage_verified()?;
        let current_preview = Self::inspect_derived_indexes(source)?;
        if current_preview.confirmation != source_preview.confirmation {
            return Err(TimelineError::ConcurrentWrite);
        }
        #[cfg(unix)]
        source_identity.verify(source)?;

        fs::OpenOptions::new()
            .write(true)
            .open(&staged.0)
            .and_then(|file| file.sync_all())
            .map_err(io_error)?;
        fs::hard_link(&staged.0, destination).map_err(io_error)?;
        if let Err(error) = sync_parent(parent, destination) {
            return Err(TimelineError::SnapshotPublicationUncertain {
                destination: destination.to_path_buf(),
                cause: error.to_string(),
            });
        }
        Ok(VerifiedTimelineBackup {
            revision: staged_preview.revision,
            canonical_events: staged_preview.canonical_events,
            source_fingerprint: staged_preview.confirmation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::domain::{BranchId, SessionId, TimelineRepository};

    struct Fixture {
        directory: PathBuf,
        source: PathBuf,
        destination: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("neuromita-snapshot-publish-{}", Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            let source = directory.join("source.sqlite3");
            let destination = directory.join("destination.sqlite3");
            let mut repo = SqliteTimelineRepository::open(&source).unwrap();
            repo.create_session(SessionId::new(), BranchId::new())
                .unwrap();
            Self {
                directory,
                source,
                destination,
            }
        }

        fn assert_no_stage(&self) {
            assert!(
                fs::read_dir(&self.directory)
                    .unwrap()
                    .map(Result::unwrap)
                    .all(|entry| !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".neuromita-timeline-backup-"))
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn failure_after_verified_stage_never_publishes_and_cleans_stage() {
        let fixture = Fixture::new();
        let fingerprint =
            SqliteTimelineRepository::inspect_derived_indexes(&fixture.source).unwrap();
        let result = SqliteTimelineRepository::publish_verified_snapshot_with(
            &fixture.source,
            &fixture.destination,
            None,
            || {
                let stage = fs::read_dir(&fixture.directory)
                    .unwrap()
                    .map(Result::unwrap)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".neuromita-timeline-backup-")
                    })
                    .expect("verified snapshot must exist before publication")
                    .path();
                let preview = SqliteTimelineRepository::inspect_derived_indexes(stage).unwrap();
                assert_eq!(preview.confirmation, fingerprint.confirmation);
                Err(TimelineError::Invalid("injected prepublication failure"))
            },
        );
        assert!(matches!(result, Err(TimelineError::Invalid(_))));
        assert!(!fixture.destination.exists());
        fixture.assert_no_stage();
        assert_eq!(
            SqliteTimelineRepository::inspect_derived_indexes(&fixture.source)
                .unwrap()
                .confirmation,
            fingerprint.confirmation
        );
    }

    #[test]
    fn destination_race_does_not_clobber_new_foreign_file() {
        let fixture = Fixture::new();
        let result = SqliteTimelineRepository::publish_verified_snapshot_with(
            &fixture.source,
            &fixture.destination,
            None,
            || {
                fs::write(&fixture.destination, b"winner of destination race").unwrap();
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(&fixture.destination).unwrap(),
            b"winner of destination race"
        );
        fixture.assert_no_stage();
    }

    #[cfg(unix)]
    #[test]
    fn replacing_source_with_identical_bytes_during_snapshot_is_rejected() {
        let fixture = Fixture::new();
        let original = SqliteTimelineRepository::inspect_derived_indexes(&fixture.source).unwrap();
        let replacement = fixture.directory.join("replacement.sqlite3");
        fs::copy(&fixture.source, &replacement).unwrap();
        let result = SqliteTimelineRepository::publish_verified_snapshot_with(
            &fixture.source,
            &fixture.destination,
            None,
            || {
                fs::rename(&replacement, &fixture.source).unwrap();
                Ok(())
            },
        );
        assert!(matches!(result, Err(TimelineError::ConcurrentWrite)));
        assert!(!fixture.destination.exists());
        fixture.assert_no_stage();
        assert_eq!(
            SqliteTimelineRepository::inspect_derived_indexes(&fixture.source)
                .unwrap()
                .confirmation,
            original.confirmation
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_sidecar_cleanup_preserves_non_utf8_filename_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let fixture = Fixture::new();
        let mut file_name = std::ffi::OsString::from_vec(b".snapshot-\xff.tmp".to_vec());
        let stage = fixture.directory.join(&file_name);
        fs::write(&stage, b"staged").unwrap();
        file_name.push("-wal");
        let wal = fixture.directory.join(&file_name);
        fs::write(&wal, b"wal").unwrap();
        file_name = std::ffi::OsString::from_vec(b".snapshot-\xff.tmp-shm".to_vec());
        let shm = fixture.directory.join(&file_name);
        fs::write(&shm, b"shm").unwrap();
        drop(StagedBackup(stage.clone()));
        assert!(!stage.exists());
        assert!(!wal.exists());
        assert!(!shm.exists());
    }

    #[test]
    fn directory_sync_failure_keeps_published_valid_snapshot_for_inspection() {
        let fixture = Fixture::new();
        let original = SqliteTimelineRepository::inspect_derived_indexes(&fixture.source).unwrap();
        let result = SqliteTimelineRepository::publish_verified_snapshot_with_sync(
            &fixture.source,
            &fixture.destination,
            None,
            || Ok(()),
            |parent, destination| {
                assert_eq!(parent, destination.parent().unwrap());
                assert!(destination.is_file());
                Err(io::Error::other("injected directory fsync failure"))
            },
        );
        assert!(matches!(
            result,
            Err(TimelineError::SnapshotPublicationUncertain { destination, cause })
                if destination == fixture.destination && cause.contains("injected directory fsync failure")
        ));
        fixture.assert_no_stage();
        let inspected = SqliteTimelineRepository::inspect_verified_snapshot(&fixture.destination)
            .expect("linked snapshot must remain inspectable after an uncertain sync");
        assert_eq!(inspected.source_fingerprint, original.confirmation);
    }

    #[test]
    fn directory_sync_failure_never_deletes_concurrently_replaced_destination() {
        let fixture = Fixture::new();
        let outsider = fixture.directory.join("other-party.txt");
        fs::write(&outsider, b"unrelated writer's file").unwrap();
        let result = SqliteTimelineRepository::publish_verified_snapshot_with_sync(
            &fixture.source,
            &fixture.destination,
            None,
            || Ok(()),
            |_, destination| {
                assert!(destination.is_file());
                fs::rename(&outsider, destination).unwrap();
                Err(io::Error::other(
                    "injected fsync failure following replacement",
                ))
            },
        );
        assert!(matches!(
            result,
            Err(TimelineError::SnapshotPublicationUncertain { .. })
        ));
        assert_eq!(
            fs::read(&fixture.destination).unwrap(),
            b"unrelated writer's file"
        );
        fixture.assert_no_stage();
    }

    #[test]
    fn independent_inspection_rejects_derived_drift_without_writing() {
        let fixture = Fixture::new();
        let first = SqliteTimelineRepository::inspect_verified_snapshot(&fixture.source).unwrap();
        assert_eq!(first.canonical_events, 0);
        let second = SqliteTimelineRepository::inspect_verified_snapshot(&fixture.source).unwrap();
        assert_eq!(first, second);
        let connection = Connection::open_existing(&fixture.source).unwrap();
        connection.exec("DROP TABLE timeline_observers;").unwrap();
        drop(connection);
        assert!(SqliteTimelineRepository::inspect_verified_snapshot(&fixture.source).is_err());
        fixture.assert_no_stage();
    }
}
