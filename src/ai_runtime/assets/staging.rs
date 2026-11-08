//! Owned, crash-reclaimable staging for model asset publication.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use uuid::Uuid;

use super::ModelAssetError;

const STAGE_PREFIX: &str = ".staging-";

pub(super) struct AssetStage {
    pub(super) path: PathBuf,
    lock_path: PathBuf,
    _owner: File,
    published: bool,
}

impl AssetStage {
    fn lock_path(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_os_string();
        name.push(".lock");
        PathBuf::from(name)
    }

    pub(super) fn recover(parent: &Path) -> Result<(), ModelAssetError> {
        for entry in std::fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(uuid) = name
                .to_str()
                .and_then(|name| name.strip_prefix(STAGE_PREFIX))
            else {
                continue;
            };
            if Uuid::parse_str(uuid).is_err() || !entry.file_type()?.is_dir() {
                continue;
            }
            let stage = entry.path();
            let marker = Self::lock_path(&stage);
            let Ok(metadata) = std::fs::symlink_metadata(&marker) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() != 0 {
                continue;
            }
            let owner = OpenOptions::new().read(true).write(true).open(&marker)?;
            let opened = owner.metadata()?;
            let current = std::fs::symlink_metadata(&marker)?;
            if !current.is_file() || current.len() != 0 || !same_file(&opened, &current) {
                return Err(ModelAssetError::Invalid(
                    "asset staging owner changed".into(),
                ));
            }
            match owner.try_lock() {
                Ok(()) => {
                    std::fs::remove_dir_all(&stage)?;
                    std::fs::remove_file(&marker)?;
                }
                Err(std::fs::TryLockError::WouldBlock) => (),
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(super) fn new(parent: &Path) -> Result<Self, ModelAssetError> {
        let path = parent.join(format!("{STAGE_PREFIX}{}", Uuid::new_v4()));
        let lock_path = Self::lock_path(&path);
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        if let Err(error) = owner.lock() {
            let _ = std::fs::remove_file(&lock_path);
            return Err(error.into());
        }
        if let Err(error) = std::fs::create_dir(&path) {
            let _ = std::fs::remove_file(&lock_path);
            return Err(error.into());
        }
        Ok(Self {
            path,
            lock_path,
            _owner: owner,
            published: false,
        })
    }

    pub(super) fn published(&mut self) {
        self.published = true;
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

#[cfg(unix)]
fn same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    first.dev() == second.dev() && first.ino() == second.ino()
}

#[cfg(not(unix))]
fn same_file(_first: &std::fs::Metadata, _second: &std::fs::Metadata) -> bool {
    true
}

impl Drop for AssetStage {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), %error, "asset staging cleanup failed");
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.lock_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.lock_path.display(), %error, "asset staging lock cleanup failed");
        }
    }
}
