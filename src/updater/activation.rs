//! Recoverable single-file activation for an already verified, unpacked release payload.
//!
//! The caller must invoke `apply` only from a separate updater process after
//! the target process has stopped. Archive extraction is outside this boundary.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{StagedArtifact, UpdateAsset};
use crate::portable::PortableDataRoot;

const WORKSPACE: &str = ".neuromita-update";
const CANDIDATE: &str = "candidate";
const BACKUP: &str = "backup";
const JOURNAL: &str = "activation.json";
const LOCK: &str = ".neuromita-update.lock";
const READY: &str = "readiness.json";
const JOURNAL_PENDING: &str = ".activation-journal.pending";
const ROLLBACK_MARKER: &str = "rollback.intent";

#[derive(Debug, Error)]
pub enum ActivationError {
    #[error("invalid or unsafe application executable target")]
    UnsafeTarget,
    #[error("an existing update transaction requires recovery or operator inspection")]
    ExistingTransaction,
    #[error("no prepared application update exists")]
    NotPrepared,
    #[error("update transaction conflicts with observed installation state: {0}")]
    Conflict(&'static str),
    #[error("update payload or installation bytes do not match their recorded digest: {0}")]
    InvalidDigest(&'static str),
    #[error("update activation I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid update transaction journal: {0}")]
    Journal(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    NoTransaction,
    Prepared,
    PreviousRestored,
    NewVersionVerified,
    AwaitingHealth,
}

#[derive(Debug, Serialize, Deserialize)]
struct ActivationJournal {
    target_name: String,
    previous_size: u64,
    previous_sha256: String,
    next_size: u64,
    next_sha256: String,
    #[serde(default)]
    health_nonce: Option<Uuid>,
}

pub struct SingleFileActivator {
    install_root: PathBuf,
    data_root: PathBuf,
    target_name: String,
}

impl SingleFileActivator {
    pub fn new(
        install_root: &Path,
        data_root: &PortableDataRoot,
        target_name: &str,
    ) -> Result<Self, ActivationError> {
        let lower_name = target_name.to_ascii_lowercase();
        if target_name.is_empty()
            || target_name.len() > 255
            || target_name.starts_with('.')
            || target_name.ends_with(['.', ' '])
            || target_name.contains(['\\', ':', '\0'])
            || target_name.chars().any(char::is_control)
            || !matches!(
                lower_name.as_str(),
                "neuromita" | "neuromita-gui" | "neuromita.exe" | "neuromita-gui.exe"
            )
            || !matches!(
                Path::new(target_name)
                    .components()
                    .collect::<Vec<_>>()
                    .as_slice(),
                [Component::Normal(_)]
            )
        {
            return Err(ActivationError::UnsafeTarget);
        }
        let root = fs::canonicalize(install_root)?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(ActivationError::UnsafeTarget);
        }
        let target = root.join(target_name);
        let user_data_root = fs::canonicalize(data_root.path())?;
        if target.starts_with(&user_data_root) {
            return Err(ActivationError::UnsafeTarget);
        }
        match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(ActivationError::UnsafeTarget),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let workspace = root.join(WORKSPACE);
                let workspace_metadata = fs::symlink_metadata(&workspace)?;
                if !workspace_metadata.is_dir() || workspace_metadata.file_type().is_symlink() {
                    return Err(ActivationError::UnsafeTarget);
                }
                regular_file(&workspace.join(JOURNAL))?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(Self {
            install_root: root,
            data_root: data_root.path().to_path_buf(),
            target_name: target_name.to_owned(),
        })
    }

    pub fn install_root(&self) -> &Path {
        &self.install_root
    }

    pub fn target_path(&self) -> PathBuf {
        self.target()
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn prepare(
        &self,
        asset: &UpdateAsset,
        artifact: &StagedArtifact,
    ) -> Result<(), ActivationError> {
        if asset.name() != self.target_name {
            return Err(ActivationError::UnsafeTarget);
        }
        self.prepare_verified(asset, artifact)
    }

    /// Map only the exact publisher-named native GUI release to its installed filename.
    /// General callers cannot use this to rename arbitrary release assets.
    pub fn prepare_native_gui(
        &self,
        asset: &UpdateAsset,
        artifact: &StagedArtifact,
    ) -> Result<(), ActivationError> {
        let installed_name = if cfg!(windows) {
            "neuromita-gui.exe"
        } else {
            "neuromita-gui"
        };
        if self.target_name != installed_name
            || Some(asset.name())
                != super::check::native_asset_name(std::env::consts::OS, std::env::consts::ARCH)
        {
            return Err(ActivationError::UnsafeTarget);
        }
        self.prepare_verified(asset, artifact)
    }

    fn prepare_verified(
        &self,
        asset: &UpdateAsset,
        artifact: &StagedArtifact,
    ) -> Result<(), ActivationError> {
        if asset.size() != artifact.size() || asset.sha256() != artifact.sha256() {
            return Err(ActivationError::UnsafeTarget);
        }
        let expected_stage = self
            .data_root
            .join("updates")
            .join("staged")
            .join(format!("sha256-{}.artifact", artifact.sha256()));
        if artifact.path() != expected_stage
            || !valid_digest(artifact.sha256())
            || artifact.size() == 0
        {
            return Err(ActivationError::UnsafeTarget);
        }
        regular_file(artifact.path())?;
        verify(
            artifact.path(),
            artifact.size(),
            artifact.sha256(),
            "staged",
        )?;
        let target = self.target();
        regular_file(&target)?;
        let (old_size, old_digest) = hash_file(&target)?;
        if old_size == 0 {
            return Err(ActivationError::InvalidDigest("installed"));
        }
        let _lock = lock_installation(&self.install_root)?;
        let workspace = self.workspace();
        match fs::create_dir(&workspace) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(ActivationError::ExistingTransaction);
            }
            Err(error) => return Err(error.into()),
        }
        let attempt = (|| -> Result<(), ActivationError> {
            let candidate = workspace.join(CANDIDATE);
            let mut input = File::open(artifact.path())?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)?;
            io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            drop(output);
            fs::set_permissions(&candidate, fs::metadata(&target)?.permissions())?;
            File::open(&candidate)?.sync_all()?;
            verify(&candidate, artifact.size(), artifact.sha256(), "candidate")?;
            let journal = ActivationJournal {
                target_name: self.target_name.clone(),
                previous_size: old_size,
                previous_sha256: old_digest,
                next_size: artifact.size(),
                next_sha256: artifact.sha256().to_owned(),
                health_nonce: None,
            };
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(workspace.join(JOURNAL))?;
            file.write_all(&serde_json::to_vec(&journal)?)?;
            file.sync_all()?;
            sync_directory(&workspace)?;
            Ok(())
        })();
        if attempt.is_err() {
            let _ = fs::remove_file(workspace.join(JOURNAL));
            let _ = fs::remove_file(workspace.join(CANDIDATE));
            let _ = fs::remove_dir(&workspace);
        }
        attempt
    }

    /// Discard a prepared, unapplied transaction when launching the helper fails.
    /// A modified target or a partially-applied transaction is never discarded.
    pub fn discard_prepared(&self) -> Result<(), ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        if self.recover_unlocked()? != RecoveryOutcome::Prepared {
            return Err(ActivationError::Conflict(
                "only an unapplied, fully verified candidate can be discarded",
            ));
        }
        self.cleanup()
    }

    /// Bind an unapplied update to exactly one helper restart attempt.
    /// A recoverable backup then remains on disk until an explicit readiness receipt.
    pub fn arm_health_check(&self, nonce: Uuid) -> Result<(), ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        if self.recover_unlocked()? != RecoveryOutcome::Prepared {
            return Err(ActivationError::NotPrepared);
        }
        let mut journal = self.load_journal()?;
        if journal.health_nonce.is_some() {
            return Err(ActivationError::ExistingTransaction);
        }
        journal.health_nonce = Some(nonce);
        let pending = self.workspace().join(JOURNAL_PENDING);
        let write = (|| -> Result<(), ActivationError> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&pending)?;
            file.write_all(&serde_json::to_vec(&journal)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&pending, self.workspace().join(JOURNAL))?;
            sync_directory(&self.workspace())?;
            Ok(())
        })();
        if write.is_err() {
            let _ = fs::remove_file(pending);
        }
        write
    }

    pub fn verify_handoff_nonce(&self, nonce: Uuid) -> Result<(), ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        if self.load_journal()?.health_nonce != Some(nonce) {
            return Err(ActivationError::Conflict("helper nonce mismatch"));
        }
        Ok(())
    }

    pub fn verify_health_nonce(&self, nonce: Uuid) -> Result<(), ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        let journal = self.load_journal()?;
        if journal.health_nonce != Some(nonce) {
            return Err(ActivationError::Conflict("health check nonce mismatch"));
        }
        if self.recover_unlocked()? != RecoveryOutcome::AwaitingHealth {
            return Err(ActivationError::Conflict(
                "new executable is not awaiting health",
            ));
        }
        Ok(())
    }

    /// Only the helper holding the verified readiness receipt may finalize the update.
    pub fn complete_health_check(&self, nonce: Uuid) -> Result<(), ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        if self.load_journal()?.health_nonce != Some(nonce)
            || self.recover_unlocked()? != RecoveryOutcome::AwaitingHealth
        {
            return Err(ActivationError::Conflict(
                "missing verified health transaction",
            ));
        }
        self.cleanup()
    }

    /// Restores the exact previous executable; refuses to overwrite third-party bytes.
    /// The new child must already have exited, especially on Windows file locks.
    pub fn rollback_health_check(&self, nonce: Uuid) -> Result<(), ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        if self.load_journal()?.health_nonce != Some(nonce)
            || self.recover_unlocked()? != RecoveryOutcome::AwaitingHealth
        {
            return Err(ActivationError::Conflict(
                "missing verified rollback transaction",
            ));
        }
        let candidate = self.workspace().join(CANDIDATE);
        if candidate.exists() {
            return Err(ActivationError::Conflict(
                "candidate occupied during rollback",
            ));
        }
        let marker_path = self.workspace().join(ROLLBACK_MARKER);
        match fs::symlink_metadata(&marker_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut marker = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker_path)?;
                marker.write_all(b"rollback")?;
                marker.sync_all()?;
                sync_directory(&self.workspace())?;
            }
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if fs::read(&marker_path)? != b"rollback" {
                    return Err(ActivationError::Conflict("invalid rollback marker"));
                }
            }
            Ok(_) => return Err(ActivationError::UnsafeTarget),
            Err(error) => return Err(error.into()),
        }
        rename_after_file_unlock(&self.target(), &candidate)?;
        sync_directory(&self.install_root)?;
        if let Err(error) = rename_after_file_unlock(&self.workspace().join(BACKUP), &self.target())
        {
            let _ = self.recover_unlocked();
            return Err(error.into());
        }
        sync_directory(&self.install_root)?;
        if self.recover_unlocked()? != RecoveryOutcome::PreviousRestored {
            return Err(ActivationError::Conflict(
                "previous executable was not restored",
            ));
        }
        Ok(())
    }

    pub fn apply(&self) -> Result<RecoveryOutcome, ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        self.apply_unlocked()
    }

    pub fn apply_or_resume(&self) -> Result<RecoveryOutcome, ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        match self.recover_unlocked()? {
            RecoveryOutcome::Prepared => self.apply_unlocked(),
            outcome => Ok(outcome),
        }
    }

    fn apply_unlocked(&self) -> Result<RecoveryOutcome, ActivationError> {
        let journal = self.load_journal()?;
        if std::env::current_exe()
            .ok()
            .and_then(|path| fs::canonicalize(path).ok())
            .is_some_and(|current| current == self.target())
        {
            return Err(ActivationError::Conflict(
                "updater cannot replace its own running executable",
            ));
        }
        self.require_previous(&journal)?;
        if self.workspace().join(BACKUP).exists() {
            return Err(ActivationError::ExistingTransaction);
        }
        verify(
            &self.workspace().join(CANDIDATE),
            journal.next_size,
            &journal.next_sha256,
            "candidate",
        )?;
        fs::rename(self.target(), self.workspace().join(BACKUP))?;
        sync_directory(&self.install_root)?;
        if let Err(error) = fs::rename(self.workspace().join(CANDIDATE), self.target()) {
            let _ = self.recover_unlocked();
            return Err(error.into());
        }
        sync_directory(&self.install_root)?;
        match self.recover_unlocked()? {
            RecoveryOutcome::NewVersionVerified => Ok(RecoveryOutcome::NewVersionVerified),
            RecoveryOutcome::AwaitingHealth => Ok(RecoveryOutcome::AwaitingHealth),
            _ => Err(ActivationError::Conflict("activation not confirmed")),
        }
    }

    pub fn recover(&self) -> Result<RecoveryOutcome, ActivationError> {
        let _lock = lock_installation(&self.install_root)?;
        self.recover_unlocked()
    }

    fn recover_unlocked(&self) -> Result<RecoveryOutcome, ActivationError> {
        match fs::symlink_metadata(self.workspace()) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RecoveryOutcome::NoTransaction);
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(ActivationError::UnsafeTarget);
            }
            Ok(_) => {}
        }
        let journal = self.load_journal()?;
        let target = self.target();
        let backup = self.workspace().join(BACKUP);
        let candidate = self.workspace().join(CANDIDATE);
        let live = if target.exists() {
            Some(hash_file(&target)?)
        } else {
            None
        };
        let old = |observed: &(u64, String)| {
            observed.0 == journal.previous_size && observed.1 == journal.previous_sha256
        };
        let new = |observed: &(u64, String)| {
            observed.0 == journal.next_size && observed.1 == journal.next_sha256
        };
        if live.as_ref().is_some_and(new) {
            if backup.exists() {
                verify(
                    &backup,
                    journal.previous_size,
                    &journal.previous_sha256,
                    "backup",
                )?;
            } else if journal.health_nonce.is_some() {
                return Err(ActivationError::Conflict(
                    "health transaction has no backup",
                ));
            }
            if journal.health_nonce.is_some() {
                return Ok(RecoveryOutcome::AwaitingHealth);
            }
            self.cleanup()?;
            return Ok(RecoveryOutcome::NewVersionVerified);
        }
        if live.as_ref().is_some_and(old) && !backup.exists() {
            if candidate.exists() {
                verify(
                    &candidate,
                    journal.next_size,
                    &journal.next_sha256,
                    "candidate",
                )?;
                if !self.workspace().join(ROLLBACK_MARKER).exists() {
                    return Ok(RecoveryOutcome::Prepared);
                }
            }
            self.cleanup()?;
            return Ok(RecoveryOutcome::PreviousRestored);
        }
        if live.is_none() && backup.exists() {
            verify(
                &backup,
                journal.previous_size,
                &journal.previous_sha256,
                "backup",
            )?;
            fs::rename(backup, target)?;
            sync_directory(&self.install_root)?;
            self.cleanup()?;
            return Ok(RecoveryOutcome::PreviousRestored);
        }
        Err(ActivationError::Conflict(
            "target and backup cannot be reconciled without touching unknown bytes",
        ))
    }

    fn require_previous(&self, journal: &ActivationJournal) -> Result<(), ActivationError> {
        verify(
            &self.target(),
            journal.previous_size,
            &journal.previous_sha256,
            "installed",
        )
    }

    fn load_journal(&self) -> Result<ActivationJournal, ActivationError> {
        let workspace = self.workspace();
        match fs::symlink_metadata(&workspace) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(ActivationError::UnsafeTarget),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ActivationError::NotPrepared);
            }
            Err(error) => return Err(error.into()),
        }
        let file = workspace.join(JOURNAL);
        regular_file(&file)?;
        let bytes = fs::read(file)?;
        if bytes.len() > 2048 {
            return Err(ActivationError::Conflict("journal too large"));
        }
        let journal: ActivationJournal = serde_json::from_slice(&bytes)?;
        if journal.target_name != self.target_name
            || !valid_digest(&journal.previous_sha256)
            || !valid_digest(&journal.next_sha256)
            || journal.next_size == 0
            || journal.previous_size == 0
        {
            return Err(ActivationError::Conflict("invalid journal identity"));
        }
        Ok(journal)
    }

    fn cleanup(&self) -> Result<(), ActivationError> {
        let workspace = self.workspace();
        for name in [BACKUP, CANDIDATE, READY, JOURNAL_PENDING, ROLLBACK_MARKER] {
            let path = workspace.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    fs::remove_file(path)?;
                }
                Ok(_) => return Err(ActivationError::Conflict("unexpected workspace entry")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        fs::remove_file(workspace.join(JOURNAL))?;
        fs::remove_dir(workspace)?;
        sync_directory(&self.install_root)?;
        Ok(())
    }

    fn target(&self) -> PathBuf {
        self.install_root.join(&self.target_name)
    }

    fn workspace(&self) -> PathBuf {
        self.install_root.join(WORKSPACE)
    }
}

fn rename_after_file_unlock(source: &Path, target: &Path) -> io::Result<()> {
    let start = Instant::now();
    loop {
        match fs::rename(source, target) {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                ) && start.elapsed() < Duration::from_secs(4) =>
            {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

fn lock_installation(root: &Path) -> Result<File, ActivationError> {
    let path = root.join(LOCK);
    let file = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            OpenOptions::new().read(true).write(true).open(&path)?
        }
        Ok(_) => return Err(ActivationError::UnsafeTarget),
        Err(error) if error.kind() == io::ErrorKind::NotFound => OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?,
        Err(error) => return Err(error.into()),
    };
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => {
            io::Error::new(io::ErrorKind::WouldBlock, "updater transaction locked")
        }
        fs::TryLockError::Error(error) => error,
    })?;
    Ok(file)
}

pub(crate) fn prepared_target_name(install_root: &Path) -> Result<String, ActivationError> {
    let workspace = fs::canonicalize(install_root)?.join(WORKSPACE);
    let metadata = fs::symlink_metadata(&workspace)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ActivationError::UnsafeTarget);
    }
    let journal_path = workspace.join(JOURNAL);
    regular_file(&journal_path)?;
    let bytes = fs::read(journal_path)?;
    if bytes.len() > 2048 {
        return Err(ActivationError::Conflict("journal too large"));
    }
    let journal: ActivationJournal = serde_json::from_slice(&bytes)?;
    let name = &journal.target_name;
    if !matches!(
        name.as_str(),
        "neuromita" | "neuromita-gui" | "neuromita.exe" | "neuromita-gui.exe"
    ) || !valid_digest(&journal.previous_sha256)
        || !valid_digest(&journal.next_sha256)
    {
        return Err(ActivationError::UnsafeTarget);
    }
    Ok(name.clone())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn regular_file(path: &Path) -> Result<(), ActivationError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(ActivationError::UnsafeTarget);
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<(u64, String), ActivationError> {
    regular_file(path)?;
    let mut file = File::open(path)?;
    let mut context = Context::new(&SHA256);
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or(ActivationError::Conflict("file too large"))?;
        context.update(&buffer[..count]);
    }
    Ok((size, super::hex_digest(context.finish().as_ref())))
}

fn verify(path: &Path, size: u64, digest: &str, role: &'static str) -> Result<(), ActivationError> {
    let observed = hash_file(path)?;
    if observed.0 != size || observed.1 != digest {
        return Err(ActivationError::InvalidDigest(role));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::{StagedArtifact, UpdateAsset, UpdateStager};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        install: PathBuf,
        data: PortableDataRoot,
        activation: SingleFileActivator,
        asset: UpdateAsset,
        artifact: StagedArtifact,
    }

    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "nm-activate-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let install = base.join("install");
            let data_dir = install.join("UserData");
            fs::create_dir_all(&data_dir).unwrap();
            fs::write(install.join("neuromita.exe"), b"old version").unwrap();
            fs::write(data_dir.join("settings.json"), b"private settings").unwrap();
            let data = PortableDataRoot::resolve(Some(&data_dir)).unwrap();
            let asset = UpdateAsset::new(
                "neuromita.exe",
                "https://example.invalid/new",
                11,
                &format!(
                    "sha256:{}",
                    super::super::hex_digest(
                        ring::digest::digest(&SHA256, b"new version").as_ref()
                    )
                ),
            )
            .unwrap();
            let stage = UpdateStager::new(&data).unwrap();
            fs::create_dir_all(&stage.stage_directory).unwrap();
            let name = format!("sha256-{}.artifact", asset.sha256());
            let path = stage.stage_directory.join(name);
            fs::write(&path, b"new version").unwrap();
            let artifact = StagedArtifact {
                path,
                size: asset.size(),
                sha256: asset.sha256(),
                reused: false,
            };
            let activation = SingleFileActivator::new(&install, &data, "neuromita.exe").unwrap();
            Self {
                install,
                data,
                activation,
                asset,
                artifact,
            }
        }

        fn workspace(&self) -> PathBuf {
            self.install.join(WORKSPACE)
        }
        fn target(&self) -> PathBuf {
            self.install.join("neuromita.exe")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.install.parent().unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn executable_mode_is_preserved_when_candidate_replaces_target() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        fs::set_permissions(f.target(), fs::Permissions::from_mode(0o755)).unwrap();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        assert_eq!(
            fs::metadata(f.workspace().join(CANDIDATE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        f.activation.apply().unwrap();
        assert_eq!(
            fs::metadata(f.target()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn exact_native_gui_release_can_be_prepared_without_arbitrary_rename() {
        let Some(release_name) =
            super::super::check::native_asset_name(std::env::consts::OS, std::env::consts::ARCH)
        else {
            return;
        };
        let fixture = Fixture::new();
        let target_name = if cfg!(windows) {
            "neuromita-gui.exe"
        } else {
            "neuromita-gui"
        };
        fs::rename(fixture.target(), fixture.install.join(target_name)).unwrap();
        let activator =
            SingleFileActivator::new(&fixture.install, &fixture.data, target_name).unwrap();
        let published = UpdateAsset::new(
            release_name,
            "https://example.invalid/release",
            fixture.asset.size(),
            &format!("sha256:{}", fixture.asset.sha256()),
        )
        .unwrap();
        let foreign = UpdateAsset::new(
            "PythonBuild.zip",
            "https://example.invalid/python",
            fixture.asset.size(),
            &format!("sha256:{}", fixture.asset.sha256()),
        )
        .unwrap();
        assert!(matches!(
            activator.prepare_native_gui(&foreign, &fixture.artifact),
            Err(ActivationError::UnsafeTarget)
        ));
        assert!(!fixture.workspace().exists());
        activator
            .prepare_native_gui(&published, &fixture.artifact)
            .unwrap();
        assert_eq!(activator.recover().unwrap(), RecoveryOutcome::Prepared);
        activator.discard_prepared().unwrap();
        assert_eq!(activator.recover().unwrap(), RecoveryOutcome::NoTransaction);
        assert_eq!(
            fs::read(fixture.install.join(target_name)).unwrap(),
            b"old version"
        );
        assert_eq!(
            fs::read(fixture.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
        assert!(fixture.artifact.path().is_file());
    }

    #[test]
    fn cleanup_refuses_to_discard_an_applied_transaction() {
        let fixture = Fixture::new();
        fixture
            .activation
            .prepare(&fixture.asset, &fixture.artifact)
            .unwrap();
        assert_eq!(
            fixture.activation.apply().unwrap(),
            RecoveryOutcome::NewVersionVerified
        );
        assert!(matches!(
            fixture.activation.discard_prepared(),
            Err(ActivationError::Conflict(_))
        ));
        assert_eq!(fs::read(fixture.target()).unwrap(), b"new version");
    }

    #[test]
    fn installation_lock_is_exclusive_and_recovers_after_drop() {
        let f = Fixture::new();
        let held = lock_installation(&f.install).unwrap();
        let second = lock_installation(&f.install).err().unwrap();
        assert!(
            matches!(second, ActivationError::Io(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        drop(held);
        lock_installation(&f.install).unwrap();
    }

    #[test]
    fn activation_can_resume_without_separate_recover_window() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        assert_eq!(prepared_target_name(&f.install).unwrap(), "neuromita.exe");
        assert_eq!(
            f.activation.apply_or_resume().unwrap(),
            RecoveryOutcome::NewVersionVerified
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"new version");
        assert_eq!(
            f.activation.apply_or_resume().unwrap(),
            RecoveryOutcome::NoTransaction
        );
        assert_eq!(
            fs::read(f.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
    }

    #[test]
    fn malformed_journal_refuses_helper_target_selection() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::write(
            f.workspace().join(JOURNAL),
            b"{\"target_name\":\"../../outside\"}",
        )
        .unwrap();
        assert!(prepared_target_name(&f.install).is_err());
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_activation_lock_is_rejected() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let outside = f.install.parent().unwrap().join("outside.lock");
        fs::write(&outside, b"untouched").unwrap();
        symlink(&outside, f.install.join(LOCK)).unwrap();
        assert!(matches!(
            lock_installation(&f.install),
            Err(ActivationError::UnsafeTarget)
        ));
        assert_eq!(fs::read(outside).unwrap(), b"untouched");
    }

    #[test]
    fn apply_verified_executable_preserves_user_data() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert_eq!(f.activation.recover().unwrap(), RecoveryOutcome::Prepared);
        assert_eq!(
            f.activation.apply().unwrap(),
            RecoveryOutcome::NewVersionVerified
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"new version");
        assert_eq!(
            fs::read(f.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
        assert_eq!(
            f.activation.recover().unwrap(),
            RecoveryOutcome::NoTransaction
        );
        assert!(!f.workspace().exists());
    }

    #[test]
    fn interrupted_after_backup_restores_previous_installation() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::rename(f.target(), f.workspace().join(BACKUP)).unwrap();
        let fresh_process = SingleFileActivator::new(&f.install, &f.data, "neuromita.exe").unwrap();
        assert_eq!(
            fresh_process.recover().unwrap(),
            RecoveryOutcome::PreviousRestored
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert!(!f.workspace().exists());
    }

    #[test]
    fn missing_target_without_verified_transaction_is_never_considered_installable() {
        let f = Fixture::new();
        fs::remove_file(f.target()).unwrap();
        assert!(SingleFileActivator::new(&f.install, &f.data, "neuromita.exe").is_err());
        assert!(!f.workspace().exists());
    }

    #[test]
    fn interrupted_after_activation_confirms_new_bytes() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::rename(f.target(), f.workspace().join(BACKUP)).unwrap();
        fs::rename(f.workspace().join(CANDIDATE), f.target()).unwrap();
        assert_eq!(
            f.activation.recover().unwrap(),
            RecoveryOutcome::NewVersionVerified
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"new version");
    }

    #[test]
    fn corrupt_candidate_refuses_before_old_installation_changes() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::write(f.workspace().join(CANDIDATE), b"evil bytes!").unwrap();
        assert!(matches!(
            f.activation.apply(),
            Err(ActivationError::InvalidDigest("candidate"))
        ));
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert!(!f.workspace().join(BACKUP).exists());
    }

    #[test]
    fn missing_candidate_after_backup_can_still_restore_old() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::rename(f.target(), f.workspace().join(BACKUP)).unwrap();
        fs::remove_file(f.workspace().join(CANDIDATE)).unwrap();
        assert_eq!(
            f.activation.recover().unwrap(),
            RecoveryOutcome::PreviousRestored
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }

    #[test]
    fn conflicting_external_file_is_never_replaced_during_recovery() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::rename(f.target(), f.workspace().join(BACKUP)).unwrap();
        fs::write(f.target(), b"third-party bytes").unwrap();
        assert!(matches!(
            f.activation.recover(),
            Err(ActivationError::Conflict(_))
        ));
        assert_eq!(fs::read(f.target()).unwrap(), b"third-party bytes");
        assert_eq!(
            fs::read(f.workspace().join(BACKUP)).unwrap(),
            b"old version"
        );
    }

    #[test]
    fn corrupt_backup_during_restart_recovery_preserves_evidence() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::rename(f.target(), f.workspace().join(BACKUP)).unwrap();
        fs::write(f.workspace().join(BACKUP), b"untrusted replacement").unwrap();
        let fresh = SingleFileActivator::new(&f.install, &f.data, "neuromita.exe").unwrap();
        assert!(matches!(
            fresh.recover(),
            Err(ActivationError::InvalidDigest("backup"))
        ));
        assert!(!f.target().exists());
        assert!(f.workspace().join(JOURNAL).exists());
        assert_eq!(
            fs::read(f.workspace().join(BACKUP)).unwrap(),
            b"untrusted replacement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_workspace_never_fools_recovery() {
        let f = Fixture::new();
        std::os::unix::fs::symlink(f.data.path(), f.workspace()).unwrap();
        assert!(matches!(
            f.activation.recover(),
            Err(ActivationError::UnsafeTarget)
        ));
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }

    #[test]
    fn existing_transaction_refuses_overwrite() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        assert!(matches!(
            f.activation.prepare(&f.asset, &f.artifact),
            Err(ActivationError::ExistingTransaction)
        ));
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }

    #[test]
    fn corrupt_source_refused_before_transaction_directory_created() {
        let f = Fixture::new();
        fs::write(f.artifact.path(), b"corrupt src").unwrap();
        assert!(matches!(
            f.activation.prepare(&f.asset, &f.artifact),
            Err(ActivationError::InvalidDigest("staged"))
        ));
        assert!(!f.workspace().exists());
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }

    #[test]
    fn release_asset_identity_is_mandatory_before_mutation() {
        let f = Fixture::new();
        let different = UpdateAsset::new(
            "unrelated.zip",
            "https://example.invalid/unrelated.zip",
            f.artifact.size(),
            &format!("sha256:{}", f.artifact.sha256()),
        )
        .unwrap();
        assert!(matches!(
            f.activation.prepare(&different, &f.artifact),
            Err(ActivationError::UnsafeTarget)
        ));
        assert!(!f.workspace().exists());
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }

    #[test]
    fn missing_journal_does_not_modify_existing_installation() {
        let f = Fixture::new();
        fs::create_dir(f.workspace()).unwrap();
        assert!(matches!(
            f.activation.recover(),
            Err(ActivationError::Io(_))
        ));
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert!(f.workspace().is_dir());
    }

    #[test]
    fn stopped_app_cannot_apply_a_modified_target() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        fs::write(f.target(), b"modified while downloading").unwrap();
        assert!(matches!(
            f.activation.apply(),
            Err(ActivationError::InvalidDigest("installed"))
        ));
        assert_eq!(fs::read(f.target()).unwrap(), b"modified while downloading");
        assert!(!f.workspace().join(BACKUP).exists());
    }

    #[test]
    fn installer_must_not_point_at_its_data_root() {
        let f = Fixture::new();
        let other = f.install.join("neuromita-gui.exe");
        fs::write(&other, b"private").unwrap();
        let protected = PortableDataRoot::resolve(Some(&f.install)).unwrap();
        assert!(matches!(
            SingleFileActivator::new(&f.install, &protected, "neuromita-gui.exe"),
            Err(ActivationError::UnsafeTarget)
        ));
        assert_eq!(fs::read(other).unwrap(), b"private");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_target_is_never_activated() {
        let f = Fixture::new();
        let target = f.target();
        fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(f.data.path().join("settings.json"), &target).unwrap();
        assert!(SingleFileActivator::new(&f.install, &f.data, "neuromita.exe").is_err());
        assert_eq!(
            fs::read(f.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
    }

    #[test]
    fn rejects_user_data_and_parent_traversal() {
        let f = Fixture::new();
        for name in [
            "../neuromita.exe",
            "UserData",
            ".neuromita-update",
            "nested/exe",
            "settings.json",
            "update.zip",
            "neuromita.exe/..",
        ] {
            assert!(SingleFileActivator::new(&f.install, &f.data, name).is_err());
        }
    }
    #[test]
    fn health_armed_update_retains_verified_backup_until_explicit_ready() {
        let f = Fixture::new();
        let nonce = Uuid::new_v4();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        f.activation.arm_health_check(nonce).unwrap();
        assert_eq!(
            f.activation.apply_or_resume().unwrap(),
            RecoveryOutcome::AwaitingHealth
        );
        assert_eq!(
            f.activation.recover().unwrap(),
            RecoveryOutcome::AwaitingHealth
        );
        assert_eq!(
            fs::read(f.workspace().join(BACKUP)).unwrap(),
            b"old version"
        );
        assert!(matches!(
            f.activation.complete_health_check(Uuid::new_v4()),
            Err(ActivationError::Conflict(_))
        ));
        assert!(f.workspace().join(BACKUP).is_file());
        f.activation.complete_health_check(nonce).unwrap();
        assert_eq!(
            f.activation.recover().unwrap(),
            RecoveryOutcome::NoTransaction
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"new version");
        assert_eq!(
            fs::read(f.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
    }

    #[test]
    fn failed_startup_rolls_back_only_exact_journal_bytes() {
        let f = Fixture::new();
        let nonce = Uuid::new_v4();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        f.activation.arm_health_check(nonce).unwrap();
        assert_eq!(
            f.activation.apply_or_resume().unwrap(),
            RecoveryOutcome::AwaitingHealth
        );
        assert!(matches!(
            f.activation.rollback_health_check(Uuid::new_v4()),
            Err(ActivationError::Conflict(_))
        ));
        f.activation.rollback_health_check(nonce).unwrap();
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert!(!f.workspace().exists());
        assert_eq!(
            fs::read(f.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
    }

    #[test]
    fn damaged_new_executable_refuses_rollback_and_keeps_backup_evidence() {
        let f = Fixture::new();
        let nonce = Uuid::new_v4();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        f.activation.arm_health_check(nonce).unwrap();
        f.activation.apply_or_resume().unwrap();
        fs::write(f.target(), b"third-party unknown bytes").unwrap();
        assert!(matches!(
            f.activation.rollback_health_check(nonce),
            Err(ActivationError::Conflict(_))
        ));
        assert_eq!(
            fs::read(f.workspace().join(BACKUP)).unwrap(),
            b"old version"
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"third-party unknown bytes");
    }

    #[test]
    fn interrupted_health_activation_restores_previous_executable() {
        let f = Fixture::new();
        let nonce = Uuid::new_v4();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        f.activation.arm_health_check(nonce).unwrap();
        fs::rename(f.target(), f.workspace().join(BACKUP)).unwrap();
        let recovered = SingleFileActivator::new(&f.install, &f.data, "neuromita.exe").unwrap();
        assert_eq!(
            recovered.recover().unwrap(),
            RecoveryOutcome::PreviousRestored
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert!(!f.workspace().exists());
    }

    #[test]
    fn journal_health_nonce_is_one_shot_and_prepared_abort_remains_safe() {
        let f = Fixture::new();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        let nonce = Uuid::new_v4();
        f.activation.arm_health_check(nonce).unwrap();
        assert!(matches!(
            f.activation.arm_health_check(Uuid::new_v4()),
            Err(ActivationError::ExistingTransaction)
        ));
        f.activation.discard_prepared().unwrap();
        assert_eq!(
            f.activation.recover().unwrap(),
            RecoveryOutcome::NoTransaction
        );
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
    }
    #[test]
    fn rollback_resumes_after_interruption_between_two_file_moves() {
        let f = Fixture::new();
        let nonce = Uuid::new_v4();
        f.activation.prepare(&f.asset, &f.artifact).unwrap();
        f.activation.arm_health_check(nonce).unwrap();
        assert_eq!(
            f.activation.apply_or_resume().unwrap(),
            RecoveryOutcome::AwaitingHealth
        );
        fs::write(f.workspace().join(ROLLBACK_MARKER), b"rollback").unwrap();
        fs::rename(f.target(), f.workspace().join(CANDIDATE)).unwrap();
        let fresh = SingleFileActivator::new(&f.install, &f.data, "neuromita.exe").unwrap();
        assert_eq!(fresh.recover().unwrap(), RecoveryOutcome::PreviousRestored);
        assert_eq!(fs::read(f.target()).unwrap(), b"old version");
        assert!(!f.workspace().exists());
        assert_eq!(
            fs::read(f.data.path().join("settings.json")).unwrap(),
            b"private settings"
        );
    }
}
