//! Crash-recoverable replacement of the two existing Python application entry files.
//!
//! The caller must supply extracted, independently verified regular files; this
//! module neither extracts ZIP archives nor authorizes a complete Python install.
//! The install must be idle before apply and recover. UserData, Settings, Prompts,
//! and all other installation files are outside this transaction.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    activation::{self, ActivationError},
    install_policy::{FileOwnership, PythonArchivePreflight},
};
use crate::portable::PortableDataRoot;

const WORKSPACE: &str = ".neuromita-python-core-update";
const PENDING: &str = "pending.json";
const COMMITTED: &str = "committed.json";
const TARGETS: [&str; 2] = ["Launcher.exe", "NeuroMita.pyz"];
const MAX_JOURNAL_BYTES: u64 = 4096;

#[derive(Debug, Error)]
pub enum PythonCoreActivationError {
    #[error("Python update requires an existing, regular Launcher.exe and NeuroMita.pyz")]
    UnsafeTarget,
    #[error("invalid Python package ownership or incomplete entry-file evidence")]
    InvalidPlan,
    #[error("unsafe or missing extracted Python entry files")]
    UnsafeSource,
    #[error("another update operation is already prepared")]
    ExistingTransaction,
    #[error("no prepared Python entry-file update exists")]
    NotPrepared,
    #[error("an update file differs from its recorded size or SHA-256: {0}")]
    InvalidDigest(&'static str),
    #[error("Python update cannot be safely reconciled: {0}")]
    Conflict(&'static str),
    #[error("installation lock failed: {0}")]
    Lock(#[from] ActivationError),
    #[error("Python update I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid Python update journal: {0}")]
    Journal(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, PythonCoreActivationError>;

#[derive(Clone, Debug)]
pub struct PythonCoreDigests {
    pub launcher: String,
    pub python_archive: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PythonCoreRecovery {
    NoTransaction,
    Prepared,
    PreviousRestored,
    NewVersionVerified,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileRecord {
    name: String,
    old_size: u64,
    old_sha256: String,
    new_size: u64,
    new_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    files: [FileRecord; 2],
}

pub struct PythonCoreActivator {
    install_root: PathBuf,
}

impl PythonCoreActivator {
    pub fn new(install_root: &Path, data_root: &PortableDataRoot) -> Result<Self> {
        let root = fs::canonicalize(install_root)?;
        if !fs::metadata(&root)?.is_dir() || root.starts_with(fs::canonicalize(data_root.path())?) {
            return Err(PythonCoreActivationError::UnsafeTarget);
        }
        for name in TARGETS {
            match fs::symlink_metadata(root.join(name)) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let workspace = root.join(WORKSPACE);
                    let metadata = fs::symlink_metadata(&workspace)
                        .map_err(|_| PythonCoreActivationError::UnsafeTarget)?;
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err(PythonCoreActivationError::UnsafeTarget);
                    }
                    if is_regular_or_absent(&workspace.join(PENDING))?.is_none()
                        && is_regular_or_absent(&workspace.join(COMMITTED))?.is_none()
                    {
                        return Err(PythonCoreActivationError::UnsafeTarget);
                    }
                }
                _ => return Err(PythonCoreActivationError::UnsafeTarget),
            }
        }
        Ok(Self { install_root: root })
    }

    /// Prepare both candidates while the original installation remains unchanged.
    /// `digests` must originate from a trusted, publisher-verified extraction;
    /// digest strings supplied by an untrusted caller establish no authenticity.
    pub fn prepare(
        &self,
        extracted_dir: &Path,
        plan: &PythonArchivePreflight,
        digests: &PythonCoreDigests,
    ) -> Result<()> {
        let expected = [&digests.launcher, &digests.python_archive];
        for (name, digest) in TARGETS.into_iter().zip(expected) {
            if !valid_digest(digest) {
                return Err(PythonCoreActivationError::InvalidPlan);
            }
            let entry = plan
                .files()
                .iter()
                .find(|entry| entry.relative_path() == name)
                .ok_or(PythonCoreActivationError::InvalidPlan)?;
            if entry.ownership() != FileOwnership::Application || entry.size() == 0 {
                return Err(PythonCoreActivationError::InvalidPlan);
            }
        }
        let metadata = fs::symlink_metadata(extracted_dir)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PythonCoreActivationError::UnsafeSource);
        }
        let extracted = fs::canonicalize(extracted_dir)?;
        if extracted.starts_with(&self.install_root) || self.install_root.starts_with(&extracted) {
            return Err(PythonCoreActivationError::UnsafeSource);
        }
        for (name, digest) in TARGETS.into_iter().zip(expected) {
            let size = plan
                .files()
                .iter()
                .find(|entry| entry.relative_path() == name)
                .expect("validated plan")
                .size();
            require_digest(&extracted.join(name), size, digest, "extracted")?;
        }
        let _lock = activation::lock_installation(&self.install_root)?;
        if self.install_root.join(".neuromita-update").exists() {
            return Err(PythonCoreActivationError::ExistingTransaction);
        }
        let workspace = self.workspace();
        fs::create_dir(&workspace).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                PythonCoreActivationError::ExistingTransaction
            } else {
                error.into()
            }
        })?;
        let prepared = (|| -> Result<()> {
            let files = TARGETS
                .into_iter()
                .zip(expected)
                .map(|(name, digest)| {
                    let installed = self.install_root.join(name);
                    let (old_size, old_sha256) = activation::hash_file(&installed)?;
                    let new_size = plan
                        .files()
                        .iter()
                        .find(|entry| entry.relative_path() == name)
                        .expect("validated plan")
                        .size();
                    let candidate = self.candidate(name);
                    let mut output = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&candidate)?;
                    io::copy(&mut File::open(extracted.join(name))?, &mut output)?;
                    output.sync_all()?;
                    drop(output);
                    fs::set_permissions(&candidate, fs::metadata(&installed)?.permissions())?;
                    File::open(&candidate)?.sync_all()?;
                    require_digest(&candidate, new_size, digest, "candidate")?;
                    Ok(FileRecord {
                        name: name.to_owned(),
                        old_size,
                        old_sha256,
                        new_size,
                        new_sha256: digest.to_owned(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let journal = Journal {
                files: files.try_into().expect("exactly two fixed targets"),
            };
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(workspace.join(PENDING))?;
            output.write_all(&serde_json::to_vec(&journal)?)?;
            output.sync_all()?;
            sync_directory(&workspace)?;
            Ok(())
        })();
        if prepared.is_err() {
            let _ = self.cleanup_known_files();
        }
        prepared
    }

    /// Apply only after *both* original application processes have exited.
    /// Requires an external process that stays alive throughout apply/recover.
    pub fn apply(&self) -> Result<PythonCoreRecovery> {
        let _lock = activation::lock_installation(&self.install_root)?;
        if self.install_root.join(".neuromita-update").exists() {
            return Err(PythonCoreActivationError::ExistingTransaction);
        }
        if is_regular_or_absent(&self.workspace().join(COMMITTED))?.is_some() {
            return Err(PythonCoreActivationError::ExistingTransaction);
        }
        let journal = self.load_journal(PENDING)?;
        self.validate_pending(&journal)?;
        if std::env::current_exe()
            .ok()
            .and_then(|path| fs::canonicalize(path).ok())
            .is_some_and(|path| TARGETS.iter().any(|name| path == self.target(name)))
        {
            return Err(PythonCoreActivationError::Conflict(
                "updater cannot replace its own running entry file",
            ));
        }
        let result = (|| -> Result<()> {
            for file in &journal.files {
                fs::rename(self.target(&file.name), self.backup(&file.name))?;
                sync_directory(&self.install_root)?;
                sync_directory(&self.workspace())?;
                fs::rename(self.candidate(&file.name), self.target(&file.name))?;
                sync_directory(&self.install_root)?;
                sync_directory(&self.workspace())?;
            }
            for file in &journal.files {
                require_digest(
                    &self.target(&file.name),
                    file.new_size,
                    &file.new_sha256,
                    "installed",
                )?;
            }
            fs::rename(
                self.workspace().join(PENDING),
                self.workspace().join(COMMITTED),
            )?;
            sync_directory(&self.workspace())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.recover_unlocked();
        }
        result?;
        self.finish_committed(&journal)?;
        Ok(PythonCoreRecovery::NewVersionVerified)
    }

    /// Idempotent after an interruption; pending transactions roll back the
    /// entire pair, while committed transactions finish removing old backups.
    pub fn recover(&self) -> Result<PythonCoreRecovery> {
        let _lock = activation::lock_installation(&self.install_root)?;
        self.recover_unlocked()
    }

    fn recover_unlocked(&self) -> Result<PythonCoreRecovery> {
        match fs::symlink_metadata(self.workspace()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PythonCoreRecovery::NoTransaction);
            }
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(PythonCoreActivationError::Conflict(
                    "workspace is not a directory",
                ));
            }
            Err(error) => return Err(error.into()),
        }
        let committed = self.workspace().join(COMMITTED);
        if is_regular_or_absent(&committed)?.is_some() {
            if self.workspace().join(PENDING).exists() {
                return Err(PythonCoreActivationError::Conflict("two active journals"));
            }
            let journal = self.load_journal(COMMITTED)?;
            return self
                .finish_committed(&journal)
                .map(|_| PythonCoreRecovery::NewVersionVerified);
        }
        let pending = self.workspace().join(PENDING);
        if is_regular_or_absent(&pending)?.is_none() {
            // Without a journal, a backup could be the only remaining previous copy.
            // Clean up only an empty workspace, never discard untracked backup bytes.
            fs::remove_dir(self.workspace()).map_err(|error| {
                if error.kind() == io::ErrorKind::DirectoryNotEmpty {
                    PythonCoreActivationError::Conflict("orphaned transaction without journal")
                } else {
                    error.into()
                }
            })?;
            sync_directory(&self.install_root)?;
            return Ok(PythonCoreRecovery::NoTransaction);
        }
        let journal = self.load_journal(PENDING)?;
        let states = journal
            .files
            .iter()
            .map(|file| self.inspect_rollback_state(file))
            .collect::<Result<Vec<_>>>()?;
        if states
            .iter()
            .all(|state| *state == RollbackState::Untouched)
            && journal
                .files
                .iter()
                .all(|file| self.candidate(&file.name).exists())
        {
            return Ok(PythonCoreRecovery::Prepared);
        }
        for (file, state) in journal.files.iter().zip(states) {
            match state {
                RollbackState::Untouched => {}
                RollbackState::BackupOnly => {
                    fs::rename(self.backup(&file.name), self.target(&file.name))?;
                }
                RollbackState::InstalledNew => {
                    fs::remove_file(self.target(&file.name))?;
                    fs::rename(self.backup(&file.name), self.target(&file.name))?;
                }
            }
            sync_directory(&self.install_root)?;
            sync_directory(&self.workspace())?;
        }
        self.cleanup_known_files()?;
        Ok(PythonCoreRecovery::PreviousRestored)
    }

    fn validate_pending(&self, journal: &Journal) -> Result<()> {
        for file in &journal.files {
            require_digest(
                &self.target(&file.name),
                file.old_size,
                &file.old_sha256,
                "installed",
            )?;
            require_digest(
                &self.candidate(&file.name),
                file.new_size,
                &file.new_sha256,
                "candidate",
            )?;
            if is_regular_or_absent(&self.backup(&file.name))?.is_some() {
                return Err(PythonCoreActivationError::Conflict(
                    "backup already present",
                ));
            }
        }
        Ok(())
    }

    fn inspect_rollback_state(&self, file: &FileRecord) -> Result<RollbackState> {
        let installed = observed(&self.target(&file.name))?;
        let backup = observed(&self.backup(&file.name))?;
        let candidate = observed(&self.candidate(&file.name))?;
        let is_old = |found: &(u64, String)| found.0 == file.old_size && found.1 == file.old_sha256;
        let is_new = |found: &(u64, String)| found.0 == file.new_size && found.1 == file.new_sha256;
        if candidate.as_ref().is_some_and(|found| !is_new(found)) {
            return Err(PythonCoreActivationError::InvalidDigest("candidate"));
        }
        if backup.as_ref().is_some_and(|found| !is_old(found)) {
            return Err(PythonCoreActivationError::InvalidDigest("backup"));
        }
        match (installed.as_ref(), backup.as_ref()) {
            (Some(current), None) if is_old(current) => Ok(RollbackState::Untouched),
            (None, Some(_)) => Ok(RollbackState::BackupOnly),
            (Some(current), Some(_)) if is_new(current) => Ok(RollbackState::InstalledNew),
            _ => Err(PythonCoreActivationError::Conflict(
                "live file is not old or new",
            )),
        }
    }

    fn finish_committed(&self, journal: &Journal) -> Result<()> {
        for file in &journal.files {
            require_digest(
                &self.target(&file.name),
                file.new_size,
                &file.new_sha256,
                "installed",
            )?;
            if let Some((size, digest)) = observed(&self.backup(&file.name))? {
                if (size, digest) != (file.old_size, file.old_sha256.clone()) {
                    return Err(PythonCoreActivationError::InvalidDigest("backup"));
                }
            }
            if let Some((size, digest)) = observed(&self.candidate(&file.name))? {
                if (size, digest) != (file.new_size, file.new_sha256.clone()) {
                    return Err(PythonCoreActivationError::InvalidDigest("candidate"));
                }
            }
        }
        self.cleanup_known_files()
    }

    fn load_journal(&self, name: &str) -> Result<Journal> {
        let metadata = fs::symlink_metadata(self.workspace())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(PythonCoreActivationError::UnsafeTarget);
        }
        self.validate_workspace_entries()?;
        regular(&self.workspace().join(name))?;
        let bytes = fs::read(self.workspace().join(name))?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(PythonCoreActivationError::Conflict("oversized journal"));
        }
        let journal: Journal = serde_json::from_slice(&bytes)?;
        for ((record, name), index) in journal.files.iter().zip(TARGETS).zip(0..) {
            if record.name != name
                || record.old_size == 0
                || record.new_size == 0
                || !valid_digest(&record.old_sha256)
                || !valid_digest(&record.new_sha256)
            {
                return Err(PythonCoreActivationError::Conflict(if index == 0 {
                    "invalid Launcher journal"
                } else {
                    "invalid NeuroMita journal"
                }));
            }
        }
        Ok(journal)
    }

    fn validate_workspace_entries(&self) -> Result<()> {
        let workspace = self.workspace();
        let expected_names = [
            PENDING.to_owned(),
            COMMITTED.to_owned(),
            format!("candidate-{}", TARGETS[0]),
            format!("candidate-{}", TARGETS[1]),
            format!("backup-{}", TARGETS[0]),
            format!("backup-{}", TARGETS[1]),
        ];
        // An unknown workspace entry must not be unlinked, nor may its
        // presence cause us to remove the recovery journal prematurely.
        for item in fs::read_dir(&workspace)? {
            let item = item?;
            if !expected_names
                .iter()
                .any(|name| item.file_name() == name.as_str())
            {
                return Err(PythonCoreActivationError::Conflict(
                    "unexpected workspace entry",
                ));
            }
            regular(&item.path())?;
        }
        Ok(())
    }

    fn cleanup_known_files(&self) -> Result<()> {
        let workspace = self.workspace();
        self.validate_workspace_entries()?;
        for name in TARGETS {
            for path in [self.candidate(name), self.backup(name)] {
                if is_regular_or_absent(&path)?.is_some() {
                    fs::remove_file(path)?;
                }
            }
        }
        for name in [PENDING, COMMITTED] {
            let path = workspace.join(name);
            if is_regular_or_absent(&path)?.is_some() {
                fs::remove_file(path)?;
            }
        }
        fs::remove_dir(&workspace)?;
        sync_directory(&self.install_root)?;
        Ok(())
    }

    fn workspace(&self) -> PathBuf {
        self.install_root.join(WORKSPACE)
    }
    fn target(&self, name: &str) -> PathBuf {
        self.install_root.join(name)
    }
    fn candidate(&self, name: &str) -> PathBuf {
        self.workspace().join(format!("candidate-{name}"))
    }
    fn backup(&self, name: &str) -> PathBuf {
        self.workspace().join(format!("backup-{name}"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RollbackState {
    Untouched,
    BackupOnly,
    InstalledNew,
}

fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(PythonCoreActivationError::UnsafeTarget)
    }
}

fn is_regular_or_absent(path: &Path) -> Result<Option<()>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(Some(())),
        Ok(_) => Err(PythonCoreActivationError::UnsafeTarget),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn observed(path: &Path) -> Result<Option<(u64, String)>> {
    if is_regular_or_absent(path)?.is_some() {
        Ok(Some(activation::hash_file(path)?))
    } else {
        Ok(None)
    }
}

fn require_digest(
    path: &Path,
    expected_size: u64,
    expected_sha: &str,
    role: &'static str,
) -> Result<()> {
    let (size, sha) = activation::hash_file(path)?;
    if size != expected_size || sha != expected_sha {
        Err(PythonCoreActivationError::InvalidDigest(role))
    } else {
        Ok(())
    }
}

fn valid_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn sync_directory(path: &Path) -> Result<()> {
    activation::sync_directory(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::install_policy::{
        ArchiveEntry, ArchiveEntryKind, preflight_python_archive,
    };
    use ring::digest::{SHA256, digest};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        extracted: PathBuf,
        data: PortableDataRoot,
        plan: PythonArchivePreflight,
        digests: PythonCoreDigests,
    }

    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "nm-python-core-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let root = base.join("install");
            let extracted = base.join("extracted");
            fs::create_dir_all(root.join("UserData")).unwrap();
            fs::create_dir_all(root.join("Settings")).unwrap();
            fs::create_dir_all(root.join("Prompts")).unwrap();
            fs::create_dir_all(&extracted).unwrap();
            fs::write(root.join("Launcher.exe"), b"old launcher").unwrap();
            fs::write(root.join("NeuroMita.pyz"), b"old pyz").unwrap();
            fs::write(root.join("UserData/user.json"), b"my settings").unwrap();
            fs::write(root.join("Settings/local.json"), b"my secrets").unwrap();
            fs::write(root.join("Prompts/local.txt"), b"my prompt").unwrap();
            fs::write(extracted.join("Launcher.exe"), b"new launcher").unwrap();
            fs::write(extracted.join("NeuroMita.pyz"), b"new pyz").unwrap();
            let data = PortableDataRoot::resolve(Some(&root.join("UserData"))).unwrap();
            let plan = preflight_python_archive([
                ArchiveEntry {
                    path: "Launcher.exe".into(),
                    kind: ArchiveEntryKind::File,
                    uncompressed_bytes: 12,
                },
                ArchiveEntry {
                    path: "NeuroMita.pyz".into(),
                    kind: ArchiveEntryKind::File,
                    uncompressed_bytes: 7,
                },
                ArchiveEntry {
                    path: "Settings/local.json".into(),
                    kind: ArchiveEntryKind::File,
                    uncompressed_bytes: 5,
                },
                ArchiveEntry {
                    path: "Prompts/local.txt".into(),
                    kind: ArchiveEntryKind::File,
                    uncompressed_bytes: 5,
                },
            ])
            .unwrap();
            Self {
                root,
                extracted,
                data,
                plan,
                digests: PythonCoreDigests {
                    launcher: sha(b"new launcher"),
                    python_archive: sha(b"new pyz"),
                },
            }
        }
        fn actor(&self) -> PythonCoreActivator {
            PythonCoreActivator::new(&self.root, &self.data).unwrap()
        }
        fn prepare(&self) {
            self.actor()
                .prepare(&self.extracted, &self.plan, &self.digests)
                .unwrap();
        }
        fn assert_old(&self) {
            assert_eq!(
                fs::read(self.root.join("Launcher.exe")).unwrap(),
                b"old launcher"
            );
            assert_eq!(
                fs::read(self.root.join("NeuroMita.pyz")).unwrap(),
                b"old pyz"
            );
            self.assert_private();
        }
        fn assert_private(&self) {
            assert_eq!(
                fs::read(self.root.join("UserData/user.json")).unwrap(),
                b"my settings"
            );
            assert_eq!(
                fs::read(self.root.join("Settings/local.json")).unwrap(),
                b"my secrets"
            );
            assert_eq!(
                fs::read(self.root.join("Prompts/local.txt")).unwrap(),
                b"my prompt"
            );
        }
        fn assert_new(&self) {
            assert_eq!(
                fs::read(self.root.join("Launcher.exe")).unwrap(),
                b"new launcher"
            );
            assert_eq!(
                fs::read(self.root.join("NeuroMita.pyz")).unwrap(),
                b"new pyz"
            );
            self.assert_private();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.root.parent().unwrap());
        }
    }
    fn sha(bytes: &[u8]) -> String {
        super::super::super::updater::hex_digest(digest(&SHA256, bytes).as_ref())
    }

    #[test]
    fn prepared_files_are_verified_before_any_target_replacement() {
        let f = Fixture::new();
        f.prepare();
        f.assert_old();
        assert_eq!(f.actor().recover().unwrap(), PythonCoreRecovery::Prepared);
    }

    #[test]
    fn successful_two_file_update_preserves_every_user_folder() {
        let f = Fixture::new();
        f.prepare();
        assert_eq!(
            f.actor().apply().unwrap(),
            PythonCoreRecovery::NewVersionVerified
        );
        f.assert_new();
        assert_eq!(
            f.actor().recover().unwrap(),
            PythonCoreRecovery::NoTransaction
        );
    }

    #[test]
    fn after_first_backup_rename_fresh_actor_restores_both_old_files() {
        let f = Fixture::new();
        f.prepare();
        fs::rename(
            f.root.join("Launcher.exe"),
            f.actor().backup("Launcher.exe"),
        )
        .unwrap();
        assert_eq!(
            f.actor().recover().unwrap(),
            PythonCoreRecovery::PreviousRestored
        );
        f.assert_old();
    }

    #[test]
    fn after_first_install_fresh_actor_rolls_back_all_files() {
        let f = Fixture::new();
        f.prepare();
        fs::rename(
            f.root.join("Launcher.exe"),
            f.actor().backup("Launcher.exe"),
        )
        .unwrap();
        fs::rename(
            f.actor().candidate("Launcher.exe"),
            f.root.join("Launcher.exe"),
        )
        .unwrap();
        assert_eq!(
            f.actor().recover().unwrap(),
            PythonCoreRecovery::PreviousRestored
        );
        f.assert_old();
    }

    #[test]
    fn committed_journal_finishes_cleanup_without_replacing_new_files() {
        let f = Fixture::new();
        f.prepare();
        for name in TARGETS {
            fs::rename(f.root.join(name), f.actor().backup(name)).unwrap();
            fs::rename(f.actor().candidate(name), f.root.join(name)).unwrap();
        }
        fs::rename(
            f.actor().workspace().join(PENDING),
            f.actor().workspace().join(COMMITTED),
        )
        .unwrap();
        assert_eq!(
            f.actor().recover().unwrap(),
            PythonCoreRecovery::NewVersionVerified
        );
        f.assert_new();
    }

    #[test]
    fn second_file_half_activated_in_fresh_process_restores_both_old_files() {
        let f = Fixture::new();
        f.prepare();
        for name in TARGETS {
            fs::rename(f.root.join(name), f.actor().backup(name)).unwrap();
            if name == "Launcher.exe" {
                fs::rename(f.actor().candidate(name), f.root.join(name)).unwrap();
            }
        }
        assert_eq!(
            f.actor().recover().unwrap(),
            PythonCoreRecovery::PreviousRestored
        );
        f.assert_old();
    }

    #[test]
    fn missing_journal_never_deletes_the_only_backup_copy() {
        let f = Fixture::new();
        f.prepare();
        let recovery = f.actor();
        fs::rename(
            f.root.join("Launcher.exe"),
            f.actor().backup("Launcher.exe"),
        )
        .unwrap();
        fs::remove_file(f.actor().workspace().join(PENDING)).unwrap();
        assert!(PythonCoreActivator::new(&f.root, &f.data).is_err());
        assert!(matches!(
            recovery.recover(),
            Err(PythonCoreActivationError::Conflict(_))
        ));
        assert_eq!(
            fs::read(recovery.backup("Launcher.exe")).unwrap(),
            b"old launcher"
        );
    }

    #[test]
    fn corrupted_target_refuses_rollback_without_overwriting_user_changes() {
        let f = Fixture::new();
        f.prepare();
        fs::rename(
            f.root.join("Launcher.exe"),
            f.actor().backup("Launcher.exe"),
        )
        .unwrap();
        fs::write(f.root.join("Launcher.exe"), b"somebody else").unwrap();
        assert!(matches!(
            f.actor().recover(),
            Err(PythonCoreActivationError::Conflict(_))
        ));
        assert_eq!(
            fs::read(f.root.join("Launcher.exe")).unwrap(),
            b"somebody else"
        );
        assert_eq!(fs::read(f.root.join("NeuroMita.pyz")).unwrap(), b"old pyz");
    }

    #[test]
    fn corrupt_candidate_does_not_change_installed_files() {
        let f = Fixture::new();
        f.prepare();
        fs::write(f.actor().candidate("NeuroMita.pyz"), b"tampered").unwrap();
        assert!(f.actor().apply().is_err());
        f.assert_old();
    }

    #[test]
    fn rejects_incomplete_or_different_publisher_file_evidence() {
        let f = Fixture::new();
        let bad = PythonCoreDigests {
            launcher: "0".repeat(64),
            python_archive: f.digests.python_archive.clone(),
        };
        assert!(f.actor().prepare(&f.extracted, &f.plan, &bad).is_err());
        f.assert_old();
        assert!(!f.actor().workspace().exists());
    }

    #[cfg(unix)]
    #[test]
    fn source_symlink_and_target_symlink_are_rejected() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        fs::remove_file(f.extracted.join("Launcher.exe")).unwrap();
        symlink(
            f.root.join("Launcher.exe"),
            f.extracted.join("Launcher.exe"),
        )
        .unwrap();
        assert!(
            f.actor()
                .prepare(&f.extracted, &f.plan, &f.digests)
                .is_err()
        );
        fs::remove_file(f.root.join("Launcher.exe")).unwrap();
        symlink(
            f.extracted.join("Launcher.exe"),
            f.root.join("Launcher.exe"),
        )
        .unwrap();
        assert!(PythonCoreActivator::new(&f.root, &f.data).is_err());
    }

    #[test]
    fn unknown_workspace_entry_does_not_destroy_recovery_journal() {
        let f = Fixture::new();
        f.prepare();
        fs::write(f.actor().workspace().join("unrecognized"), b"do not remove").unwrap();
        assert!(matches!(
            f.actor().recover(),
            Err(PythonCoreActivationError::Conflict(_))
        ));
        assert!(f.actor().apply().is_err());
        f.assert_old();
        assert!(f.actor().workspace().join("unrecognized").is_file());
    }

    #[test]
    fn shared_lock_rejects_concurrent_single_file_activation() {
        let f = Fixture::new();
        let lock = activation::lock_installation(&f.root).unwrap();
        assert!(
            f.actor()
                .prepare(&f.extracted, &f.plan, &f.digests)
                .is_err()
        );
        drop(lock);
        f.prepare();
    }
}
