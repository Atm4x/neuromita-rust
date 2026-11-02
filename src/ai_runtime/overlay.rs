//! Immutable model-specific dependency overlays (never contain shared backend distributions).

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

use super::layer::{LayerError, LayerInstaller, valid_segment, validate_lock};

pub type OverlayError = LayerError;

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

pub struct OverlayInstaller {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub overlays_directory: PathBuf,
    pub cache_directory: PathBuf,
    pub timeout: Duration,
}

impl LockedOverlay {
    fn validate(&self) -> Result<(), OverlayError> {
        if !valid_segment(&self.plugin_id) || !valid_segment(&self.revision) {
            return Err(LayerError::Invalid("invalid plugin ID or revision".into()));
        }
        validate_lock(&self.packages, &self.python_version)?;
        for package in &self.packages {
            let name = package
                .name
                .to_ascii_lowercase()
                .replace(['-', '_', '.'], "-");
            if [
                "torch",
                "torchaudio",
                "torchvision",
                "onnxruntime",
                "onnxruntime-gpu",
                "onnxruntime-directml",
            ]
            .contains(&name.as_str())
            {
                return Err(LayerError::Invalid(
                    "backend packages belong to a separate layer".into(),
                ));
            }
        }
        Ok(())
    }
}

impl OverlayInstaller {
    fn layer(&self) -> LayerInstaller<'_> {
        LayerInstaller {
            uv_executable: &self.uv_executable,
            python_executable: &self.python_executable,
            cache_directory: &self.cache_directory,
            timeout: self.timeout,
        }
    }

    pub fn installed_path(&self, plan: &LockedOverlay) -> Result<PathBuf, OverlayError> {
        plan.validate()?;
        if !self.overlays_directory.is_absolute() {
            return Err(LayerError::Invalid("overlay root must be absolute".into()));
        }
        Ok(self
            .overlays_directory
            .join(&plan.plugin_id)
            .join(&plan.revision))
    }

    pub async fn installed(&self, plan: &LockedOverlay) -> Result<bool, OverlayError> {
        self.layer()
            .installed(&self.installed_path(plan)?, ".neuromita-overlay.json", plan)
            .await
    }

    pub async fn install(&self, plan: &LockedOverlay) -> Result<PathBuf, OverlayError> {
        self.layer()
            .publish(
                &self.installed_path(plan)?,
                ".neuromita-overlay.json",
                plan,
                &plan.packages,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

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
            Err(LayerError::Conflict)
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
