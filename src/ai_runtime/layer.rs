//! Filesystem publication and uv execution shared by backend layers and plugin overlays.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

use super::overlay::LockedPackage;

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
        Ok(())
    }

    pub async fn installed<T: DeserializeOwned + PartialEq>(
        &self,
        destination: &Path,
        marker: &str,
        plan: &T,
    ) -> Result<bool, LayerError> {
        if !destination.exists() {
            return Ok(false);
        }
        let data = tokio::fs::read(destination.join(marker))
            .await
            .map_err(|_| LayerError::Conflict)?;
        let saved: T = serde_json::from_slice(&data).map_err(|_| LayerError::Conflict)?;
        if saved != *plan {
            return Err(LayerError::Conflict);
        }
        Ok(true)
    }

    pub async fn publish<T: Serialize + DeserializeOwned + PartialEq>(
        &self,
        destination: &Path,
        marker: &str,
        plan: &T,
        packages: &[LockedPackage],
    ) -> Result<PathBuf, LayerError> {
        self.verify_paths(destination)?;
        if self.installed(destination, marker, plan).await? {
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
        tokio::fs::create_dir(&staging).await?;
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
            let bytes =
                serde_json::to_vec_pretty(plan).map_err(|e| LayerError::Invalid(e.to_string()))?;
            tokio::fs::write(staging.join(marker), bytes).await?;
            if destination.exists() {
                return Err(LayerError::Conflict);
            }
            tokio::fs::rename(&staging, destination).await?;
            Ok(destination.to_owned())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
        }
        result
    }
}
