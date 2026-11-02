//! Versioned package overlay publication from a previously resolved dependency lock.

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedOverlay {
    pub plugin_id: String,
    pub revision: String,
    pub python_version: String,
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Error)]
pub enum OverlayError {
    #[error("invalid overlay specification: {0}")]
    Invalid(String),
    #[error("overlay already exists with different contents")]
    Conflict,
    #[error("overlay filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv overlay install timed out")]
    Timeout,
    #[error("uv overlay install failed: {0}")]
    Install(String),
}

pub struct OverlayInstaller {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub overlays_directory: PathBuf,
    pub cache_directory: PathBuf,
    pub timeout: Duration,
}

impl LockedOverlay {
    fn validate(&self) -> Result<(), OverlayError> {
        let safe_id = |value: &str| {
            !value.is_empty()
                && value.is_ascii()
                && value
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-')
                && value != "." && value != ".."
        };
        if !safe_id(&self.plugin_id) || !safe_id(&self.revision) {
            return Err(OverlayError::Invalid(
                "invalid plugin ID or revision".into(),
            ));
        }
        if self.python_version.is_empty()
            || !self
                .python_version
                .bytes()
                .all(|c| c.is_ascii_digit() || c == b'.')
        {
            return Err(OverlayError::Invalid("invalid Python version".into()));
        }
        if self.packages.len() > 256 {
            return Err(OverlayError::Invalid("package limit exceeded".into()));
        }
        for package in &self.packages {
            if package.name.is_empty()
                || !package.name.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'.' || ch == b'-')
                || package.version.is_empty()
                || !package.version.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'.' || ch == b'+' || ch == b'-' || ch == b'_')
                || !package.version.bytes().next().unwrap().is_ascii_digit()
            {
                return Err(OverlayError::Invalid("invalid locked distribution".into()));
            }
            if [
                "torch", "torchaudio", "torchvision", "onnxruntime",
                "onnxruntime-gpu", "onnxruntime-directml",
            ].contains(&package.name.to_ascii_lowercase().as_str()) {
                return Err(OverlayError::Invalid(
                    "backend packages belong to a separate layer".into(),
                ));
            }
        }
        Ok(())
    }
}

impl OverlayInstaller {
    pub fn installed_path(&self, plan: &LockedOverlay) -> Result<PathBuf, OverlayError> {
        plan.validate()?;
        if !self.overlays_directory.is_absolute() {
            return Err(OverlayError::Invalid(
                "overlay root must be absolute".into(),
            ));
        }
        Ok(self.overlays_directory.join(&plan.plugin_id).join(&plan.revision))
    }

    pub async fn installed(&self, plan: &LockedOverlay) -> Result<bool, OverlayError> {
        let destination = self.installed_path(plan)?;
        let manifest = destination.join(".neuromita-overlay.json");
        if !destination.exists() {
            return Ok(false);
        }
        let bytes = tokio::fs::read(manifest)
            .await
            .map_err(|_| OverlayError::Conflict)?;
        let saved: LockedOverlay =
            serde_json::from_slice(&bytes).map_err(|_| OverlayError::Conflict)?;
        if saved != *plan {
            return Err(OverlayError::Conflict);
        }
        Ok(true)
    }

    pub async fn install(&self, plan: &LockedOverlay) -> Result<PathBuf, OverlayError> {
        let destination = self.installed_path(plan)?;
        if self.installed(plan).await? {
            return Ok(destination);
        }
        if !self.uv_executable.is_file() || !self.python_executable.is_file() {
            return Err(OverlayError::Invalid(
                "uv or managed Python is missing".into(),
            ));
        }
        if !self.uv_executable.is_absolute()
            || !self.python_executable.is_absolute()
            || !self.cache_directory.is_absolute()
        {
            return Err(OverlayError::Invalid(
                "executable and cache paths must be absolute".into(),
            ));
        }
        let parent = destination
            .parent()
            .expect("destination always has a parent");
        tokio::fs::create_dir_all(parent).await?;
        tokio::fs::create_dir_all(&self.cache_directory).await?;
        let staging = parent.join(format!(".staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&staging).await?;
        let result = async {
            if !plan.packages.is_empty() {
                let mut cmd = Command::new(&self.uv_executable);
                cmd.args([
                    "pip", "install", "--no-config", "--no-deps",
                    "--no-python-downloads", "--python",
                ])
                .arg(&self.python_executable)
                .arg("--target")
                .arg(&staging)
                .env("UV_CACHE_DIR", &self.cache_directory)
                .env_remove("VIRTUAL_ENV")
                .kill_on_drop(true);
                for package in &plan.packages {
                    cmd.arg(format!("{}=={}", package.name, package.version));
                }
                let output = timeout(self.timeout, cmd.output())
                    .await
                    .map_err(|_| OverlayError::Timeout)??;
                if !output.status.success() {
                    return Err(OverlayError::Install(
                        String::from_utf8_lossy(&output.stderr)
                            .chars().take(1200).collect(),
                    ));
                }
            }
            let bytes = serde_json::to_vec_pretty(plan)
                .map_err(|e| OverlayError::Invalid(e.to_string()))?;
            tokio::fs::write(staging.join(".neuromita-overlay.json"), bytes).await?;
            if destination.exists() {
                return Err(OverlayError::Conflict);
            }
            tokio::fs::rename(&staging, &destination).await?;
            Ok(destination.clone())
        }.await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(staging).await;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> LockedOverlay {
        LockedOverlay {
            plugin_id: "mock_tts".into(),
            revision: "rev1".into(),
            python_version: "3.13.5".into(),
            packages: vec![],
        }
    }

    #[tokio::test]
    async fn installs_empty_lock_and_rejects_mutated_revision() {
        let root = std::env::temp_dir().join(format!("nm-overlay-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        let python = root.join("python");
        tokio::fs::write(&uv, "fake").await.unwrap();
        tokio::fs::write(&python, "fake").await.unwrap();
        let installer = OverlayInstaller {
            uv_executable: uv,
            python_executable: python,
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let spec = plan();
        let first = installer.install(&spec).await.unwrap();
        assert!(installer.installed(&spec).await.unwrap());
        assert_eq!(installer.install(&spec).await.unwrap(), first);
        let mut changed = spec;
        changed.python_version = "3.13.6".into();
        assert!(matches!(
            installer.installed(&changed).await,
            Err(OverlayError::Conflict)
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn rejects_unsafe_id_and_backend_package_in_overlay() {
        let mut spec = plan();
        spec.plugin_id = "../escapes".into();
        assert!(spec.validate().is_err());
        spec.plugin_id = "mock_tts".into();
        spec.packages.push(LockedPackage {
            name: "torch".into(),
            version: "2.8.0".into(),
        });
        assert!(spec.validate().is_err());
    }
}
