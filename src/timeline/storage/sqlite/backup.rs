use std::{fs, io, path::Path};

use uuid::Uuid;

use super::{Connection, SqliteTimelineRepository, storage};
use crate::timeline::domain::TimelineError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedTimelineBackup {
    pub revision: u64,
    pub canonical_events: usize,
    pub source_fingerprint: String,
}

struct StagedBackup(std::path::PathBuf);

impl Drop for StagedBackup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        for suffix in ["-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{}", self.0.display(), suffix));
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
fn sync_directory(directory: &Path) -> Result<(), TimelineError> {
    fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), TimelineError> {
    Ok(())
}

impl SqliteTimelineRepository {
    /// Create a new, stand-alone SQLite snapshot containing the complete Timeline database.
    /// Includes committed WAL frames, validates the original and backup, and never overwrites
    /// an existing destination. Requires an offline, trusted operator; this is not a live
    /// application lock or a backup of files owned by other components.
    pub fn create_verified_backup(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<VerifiedTimelineBackup, TimelineError> {
        let source = source.as_ref();
        let destination = destination.as_ref();
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
        let source_preview = Self::inspect_derived_indexes(source)?;
        if source_preview.requires_repair() {
            return Err(storage("source derived indexes need repair before backup"));
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
        let current_preview = Self::inspect_derived_indexes(source)?;
        if current_preview.confirmation != source_preview.confirmation {
            return Err(TimelineError::ConcurrentWrite);
        }

        fs::File::open(&staged.0)
            .and_then(|file| file.sync_all())
            .map_err(io_error)?;
        fs::hard_link(&staged.0, destination).map_err(io_error)?;
        if let Err(error) = sync_directory(parent) {
            let _ = fs::remove_file(destination);
            return Err(error);
        }
        Ok(VerifiedTimelineBackup {
            revision: staged_preview.revision,
            canonical_events: staged_preview.canonical_events,
            source_fingerprint: staged_preview.confirmation,
        })
    }
}
