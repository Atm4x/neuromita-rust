//! Filesystem publication and uv execution shared by backend layers and plugin overlays.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use uuid::Uuid;

use super::{
    distribution,
    overlay::{InstallerAction, LockedPackage},
};

#[derive(Debug, Error)]
pub enum LayerError {
    #[error("invalid package layer: {0}")]
    Invalid(String),
    #[error("package layer is not installed: {0}")]
    NotInstalled(String),
    #[error("immutable revision already exists with different contents")]
    Conflict,
    #[error("package layer filesystem failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv package operation timed out")]
    Timeout,
    #[error("uv package operation failed: {0}")]
    Install(String),
    #[error("installed package layer is incomplete or corrupted: {0}")]
    Corrupted(String),
}

fn remove_abandoned_staging(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to clean abandoned Python layer staging");
    }
}

struct StagingCleanup {
    path: PathBuf,
    published: bool,
}

impl StagingCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            published: false,
        }
    }

    fn disarm(&mut self) {
        self.published = true;
    }
}

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let path = std::mem::take(&mut self.path);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || remove_abandoned_staging(&path));
        } else {
            remove_abandoned_staging(&path);
        }
    }
}

pub struct LayerInstaller<'a> {
    pub uv_executable: &'a Path,
    pub python_executable: &'a Path,
    pub cache_directory: &'a Path,
    pub timeout: Duration,
}

pub fn valid_segment(name: &str) -> bool {
    !name.is_empty()
        && name.is_ascii()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn validate_lock(packages: &[LockedPackage], python_version: &str) -> Result<(), LayerError> {
    let parts: Vec<_> = python_version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(LayerError::Invalid("pin exact Python version".into()));
    }
    if packages.len() > 256 {
        return Err(LayerError::Invalid("package limit exceeded".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for p in packages {
        if p.name.is_empty()
            || !p
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
            || p.version.is_empty()
            || !p.version.bytes().next().unwrap().is_ascii_digit()
            || !p
                .version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'-' | b'_'))
        {
            return Err(LayerError::Invalid(
                "package name/version must be exact uv-safe pins".into(),
            ));
        }
        let normalized = p.name.to_ascii_lowercase().replace(['-', '_', '.'], "-");
        if !seen.insert(normalized) {
            return Err(LayerError::Invalid("duplicate distribution".into()));
        }
    }
    Ok(())
}

pub(super) fn reject_symlinked_layer_components(destination: &Path) -> Result<(), LayerError> {
    for component in destination.ancestors().take(3) {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LayerError::Invalid(
                    "layer revision or its managed parent must not be a symlink".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(LayerError::Io(error)),
        }
    }
    Ok(())
}

pub(super) fn reject_unsafe_layer_receipt(marker: &Path) -> Result<(), LayerError> {
    match std::fs::symlink_metadata(marker) {
        Ok(metadata) if !metadata.is_file() || metadata.len() > 131_072 => {
            Err(LayerError::Conflict)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LayerError::Io(error)),
    }
}

fn safe_marker_target(root: &Path, target: &Path) -> bool {
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    if !relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return false;
    }
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component);
        if current.is_symlink() {
            return false;
        }
    }
    true
}

impl LayerInstaller<'_> {
    pub fn verify_paths(&self, destination: &Path) -> Result<(), LayerError> {
        if !destination.is_absolute()
            || !self.uv_executable.is_absolute()
            || !self.python_executable.is_absolute()
            || !self.cache_directory.is_absolute()
            || self.timeout.is_zero()
        {
            return Err(LayerError::Invalid(
                "absolute paths and nonzero timeout required".into(),
            ));
        }
        reject_symlinked_layer_components(destination)?;
        Ok(())
    }

    pub async fn installed<T: DeserializeOwned + PartialEq>(
        &self,
        destination: &Path,
        marker: &str,
        plan: &T,
        packages: &[LockedPackage],
        actions: &[InstallerAction],
    ) -> Result<bool, LayerError> {
        reject_symlinked_layer_components(destination)?;
        if !destination.exists() {
            return Ok(false);
        }
        let marker_path = destination.join(marker);
        reject_unsafe_layer_receipt(&marker_path)?;
        let data = tokio::fs::read(marker_path)
            .await
            .map_err(|_| LayerError::Conflict)?;
        let saved: T = serde_json::from_slice(&data).map_err(|_| LayerError::Conflict)?;
        if saved != *plan {
            return Err(LayerError::Conflict);
        }
        distribution::inspect(destination, packages).map_err(LayerError::Corrupted)?;
        for action in actions {
            let target = destination.join(&action.relative_path);
            if !safe_marker_target(destination, &target)
                || !tokio::fs::metadata(&target)
                    .await
                    .map_err(|_| LayerError::Corrupted(action.id.clone()))?
                    .is_file()
                || tokio::fs::metadata(&target).await?.len() != 0
            {
                return Err(LayerError::Corrupted(action.id.clone()));
            }
        }
        Ok(true)
    }

    pub async fn publish<T: Serialize + DeserializeOwned + PartialEq>(
        &self,
        destination: &Path,
        marker: &str,
        plan: &T,
        packages: &[LockedPackage],
        actions: &[InstallerAction],
    ) -> Result<PathBuf, LayerError> {
        self.verify_paths(destination)?;
        if self
            .installed(destination, marker, plan, packages, actions)
            .await?
        {
            return Ok(destination.to_owned());
        }
        if !self.uv_executable.is_file() || !self.python_executable.is_file() {
            return Err(LayerError::Invalid(
                "uv or managed Python is missing".into(),
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| LayerError::Invalid("missing parent".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::create_dir_all(self.cache_directory).await?;
        let staging = parent.join(format!(".staging-{}", Uuid::new_v4()));
        std::fs::create_dir(&staging)?;
        let mut cleanup = StagingCleanup::new(staging.clone());
        let result = async {
            if !packages.is_empty() {
                let mut cmd = Command::new(self.uv_executable);
                cmd.args([
                    "pip",
                    "install",
                    "--no-config",
                    "--no-deps",
                    "--no-python-downloads",
                    "--python",
                ])
                .arg(self.python_executable)
                .arg("--target")
                .arg(&staging)
                .env("UV_CACHE_DIR", self.cache_directory)
                .env_remove("VIRTUAL_ENV")
                .kill_on_drop(true);
                for package in packages {
                    cmd.arg(format!("{}=={}", package.name, package.version));
                }
                let output = timeout(self.timeout, cmd.output())
                    .await
                    .map_err(|_| LayerError::Timeout)??;
                if !output.status.success() {
                    return Err(LayerError::Install(
                        String::from_utf8_lossy(&output.stderr)
                            .chars()
                            .take(1200)
                            .collect(),
                    ));
                }
            }
            distribution::inspect(&staging, packages).map_err(LayerError::Corrupted)?;
            for action in actions {
                if action.kind != "create_empty_marker" {
                    return Err(LayerError::Invalid("unsupported installer action".into()));
                }
                let target = staging.join(&action.relative_path);
                let parent = target
                    .parent()
                    .ok_or_else(|| LayerError::Invalid("invalid marker parent".into()))?;
                if !parent.is_dir() || !safe_marker_target(&staging, &target) || target.exists() {
                    return Err(LayerError::Invalid(
                        "marker location is not a pristine installed package".into(),
                    ));
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)
                    .await?;
                file.flush().await?;
            }
            let bytes =
                serde_json::to_vec_pretty(plan).map_err(|e| LayerError::Invalid(e.to_string()))?;
            tokio::fs::write(staging.join(marker), bytes).await?;
            if destination.exists() {
                if self
                    .installed(destination, marker, plan, packages, actions)
                    .await?
                {
                    return Ok(destination.to_owned());
                }
                return Err(LayerError::Conflict);
            }
            match std::fs::rename(&staging, destination) {
                Ok(()) => {
                    cleanup.disarm();
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    if self
                        .installed(destination, marker, plan, packages, actions)
                        .await?
                    {
                        return Ok(destination.to_owned());
                    }
                    return Err(LayerError::Conflict);
                }
                Err(error) => return Err(LayerError::Io(error)),
            }
            Ok(destination.to_owned())
        }
        .await;
        if result.is_err() && tokio::fs::remove_dir_all(&staging).await.is_ok() {
            cleanup.disarm();
        }
        result
    }
}
