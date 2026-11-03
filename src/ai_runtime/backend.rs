//! Shared, versioned PyTorch and ONNX Runtime layers, installed with an immutable lock.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::{
    layer::{LayerError, LayerInstaller, valid_segment, validate_lock},
    overlay::LockedPackage,
};

pub type BackendError = LayerError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    TorchCpu,
    TorchCuda,
    OnnxCpu,
    OnnxCuda,
    OnnxDirectml,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TorchCpu => "torch_cpu",
            Self::TorchCuda => "torch_cuda",
            Self::OnnxCpu => "onnx_cpu",
            Self::OnnxCuda => "onnx_cuda",
            Self::OnnxDirectml => "onnx_directml",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "torch_cpu" => Some(Self::TorchCpu),
            "torch_cuda" => Some(Self::TorchCuda),
            "onnx_cpu" => Some(Self::OnnxCpu),
            "onnx_cuda" => Some(Self::OnnxCuda),
            "onnx_directml" => Some(Self::OnnxDirectml),
            _ => None,
        }
    }

    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::TorchCpu => "torch-cpu",
            Self::TorchCuda => "torch-cuda",
            Self::OnnxCpu => "onnx-cpu",
            Self::OnnxCuda => "onnx-cuda",
            Self::OnnxDirectml => "onnx-directml",
        }
    }
    fn required_package(self) -> &'static str {
        match self {
            Self::TorchCpu | Self::TorchCuda => "torch",
            Self::OnnxCpu => "onnxruntime",
            Self::OnnxCuda => "onnxruntime-gpu",
            Self::OnnxDirectml => "onnxruntime-directml",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedBackend {
    pub kind: BackendKind,
    pub revision: String,
    pub python_version: String,
    pub packages: Vec<LockedPackage>,
}

pub struct BackendInstaller {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub backends_directory: PathBuf,
    pub cache_directory: PathBuf,
    pub timeout: Duration,
}

impl LockedBackend {
    pub(crate) fn validate(&self) -> Result<(), BackendError> {
        if !valid_segment(&self.revision) {
            return Err(LayerError::Invalid("invalid backend revision".into()));
        }
        validate_lock(&self.packages, &self.python_version)?;
        let names: Vec<_> = self
            .packages
            .iter()
            .map(|p| p.name.to_ascii_lowercase().replace(['-', '_', '.'], "-"))
            .collect();
        if !names
            .iter()
            .any(|name| name == self.kind.required_package())
        {
            return Err(LayerError::Invalid(format!(
                "backend must pin {}",
                self.kind.required_package()
            )));
        }
        let onnx_variants = ["onnxruntime", "onnxruntime-gpu", "onnxruntime-directml"];
        if names
            .iter()
            .filter(|name| onnx_variants.contains(&name.as_str()))
            .count()
            > 1
        {
            return Err(LayerError::Invalid(
                "multiple ONNX runtime variants in one backend layer".into(),
            ));
        }
        Ok(())
    }
}

/// Read the authoritative plan published with an installed backend layer.
/// Selecting a layer never writes or mutates it.
pub fn load_installed_backend(
    backends_directory: &Path,
    kind: BackendKind,
    revision: &str,
) -> Result<(LockedBackend, PathBuf), BackendError> {
    if !backends_directory.is_absolute() {
        return Err(LayerError::Invalid("backend root must be absolute".into()));
    }
    if !valid_segment(revision) {
        return Err(LayerError::Invalid("invalid backend revision".into()));
    }
    let path = backends_directory.join(kind.directory()).join(revision);
    let marker = path.join(".neuromita-backend.json");
    let bytes = std::fs::read(&marker).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LayerError::NotInstalled(path.display().to_string())
        } else {
            LayerError::Io(error)
        }
    })?;
    let locked: LockedBackend = serde_json::from_slice(&bytes).map_err(|_| LayerError::Conflict)?;
    locked.validate()?;
    if locked.kind != kind || locked.revision != revision {
        return Err(LayerError::Invalid(
            "installed backend does not match the requested kind and revision".into(),
        ));
    }
    Ok((locked, path))
}

impl BackendInstaller {
    fn layer(&self) -> LayerInstaller<'_> {
        LayerInstaller {
            uv_executable: &self.uv_executable,
            python_executable: &self.python_executable,
            cache_directory: &self.cache_directory,
            timeout: self.timeout,
        }
    }
    pub fn installed_path(&self, plan: &LockedBackend) -> Result<PathBuf, BackendError> {
        plan.validate()?;
        if !self.backends_directory.is_absolute() {
            return Err(LayerError::Invalid("backend root must be absolute".into()));
        }
        Ok(self
            .backends_directory
            .join(plan.kind.directory())
            .join(&plan.revision))
    }
    pub async fn installed(&self, plan: &LockedBackend) -> Result<bool, BackendError> {
        self.layer()
            .installed(&self.installed_path(plan)?, ".neuromita-backend.json", plan)
            .await
    }
    pub async fn install(&self, plan: &LockedBackend) -> Result<PathBuf, BackendError> {
        self.layer()
            .publish(
                &self.installed_path(plan)?,
                ".neuromita-backend.json",
                plan,
                &plan.packages,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use uuid::Uuid;

    fn plan() -> LockedBackend {
        LockedBackend {
            kind: BackendKind::OnnxDirectml,
            revision: "rev1".into(),
            python_version: "3.13.5".into(),
            packages: vec![LockedPackage {
                name: "onnxruntime-directml".into(),
                version: "1.22.0".into(),
            }],
        }
    }

    #[test]
    fn validates_backend_family_and_lock() {
        let mut spec = plan();
        assert!(spec.validate().is_ok());
        spec.packages[0].name = "torch".into();
        assert!(spec.validate().is_err());
        spec.packages[0].name = "onnxruntime-directml".into();
        spec.packages.push(LockedPackage {
            name: "onnxruntime-gpu".into(),
            version: "1.22.0".into(),
        });
        assert!(spec.validate().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backend_is_published_immutably_without_modifying_overlay() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("nm-backend-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        tokio::fs::write(&uv, "#!/bin/sh\n[ \"$1\" = pip ] || exit 2\nexit 0\n")
            .await
            .unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let python = root.join("python");
        tokio::fs::write(&python, "fake").await.unwrap();
        let installer = BackendInstaller {
            uv_executable: uv,
            python_executable: python,
            backends_directory: root.join("backends"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let spec = plan();
        let path = installer.install(&spec).await.unwrap();
        assert!(path.join(".neuromita-backend.json").exists());
        assert_eq!(path, installer.install(&spec).await.unwrap());
        let mut conflicting = spec;
        conflicting.packages[0].version = "1.23.0".into();
        assert!(matches!(
            installer.install(&conflicting).await,
            Err(LayerError::Conflict)
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
