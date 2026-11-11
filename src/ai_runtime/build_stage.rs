use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::{
    install::{BuildPlatform, BuildStage, BuildStageKind},
    layer::{LayerError, LayerInstaller, sha256_file, valid_segment, validate_lock},
    overlay::LockedPackage,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovedWheelStage {
    pub plugin_id: String,
    pub plugin_revision: String,
    pub python_version: String,
    pub build_stage: BuildStage,
    pub package: LockedPackage,
    pub wheel_file: String,
    pub wheel_sha256: String,
    pub install_plan_sha256: String,
    pub uv_sha256: String,
    pub python_sha256: String,
}

#[derive(Clone)]
pub struct WheelStageInstaller {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub data_directory: PathBuf,
    pub timeout: Duration,
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

fn valid_wheel_file(value: &str) -> bool {
    value.ends_with(".whl")
        && !value.contains("..")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn host_platform() -> BuildPlatform {
    if cfg!(target_os = "windows") {
        BuildPlatform::Windows
    } else if cfg!(target_os = "macos") {
        BuildPlatform::Macos
    } else {
        BuildPlatform::Linux
    }
}

fn reject_symlink_chain(path: &Path, root: &Path) -> Result<(), LayerError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| LayerError::Invalid("build input escapes AIData".into()))?;
    let mut current = root.to_owned();
    if current.is_symlink() {
        return Err(LayerError::Invalid(
            "AIData root must not be symlinked".into(),
        ));
    }
    for component in relative.components() {
        current.push(component);
        if current.is_symlink() {
            return Err(LayerError::Invalid(
                "build stage path contains symlink".into(),
            ));
        }
    }
    Ok(())
}

impl ApprovedWheelStage {
    pub fn validate(&self) -> Result<(), LayerError> {
        if !valid_segment(&self.plugin_id)
            || !valid_segment(&self.plugin_revision)
            || !valid_segment(&self.build_stage.id)
            || self.build_stage.kind != BuildStageKind::PythonWheel
            || self.build_stage.platform != host_platform()
            || !matches!(
                self.build_stage.backend.as_str(),
                "torch-cpu"
                    | "torch-cuda"
                    | "onnx-cpu"
                    | "onnx-cuda"
                    | "onnx-directml"
                    | "ctranslate2-cpu"
                    | "ctranslate2-cuda"
                    | "remote-api"
            )
            || self.build_stage.requirement.as_deref()
                != Some(format!("{}=={}", self.package.name, self.package.version).as_str())
            || !valid_wheel_file(&self.wheel_file)
            || !valid_sha256(&self.wheel_sha256)
            || !valid_sha256(&self.install_plan_sha256)
            || !valid_sha256(&self.uv_sha256)
            || !valid_sha256(&self.python_sha256)
        {
            return Err(LayerError::Invalid(
                "wheel stage requires exact approved platform, package and SHA-256 pins".into(),
            ));
        }
        validate_lock(std::slice::from_ref(&self.package), &self.python_version)?;
        let wheel_prefix = format!(
            "{}-{}-",
            self.package.name.to_ascii_lowercase().replace('-', "_"),
            self.package.version
        );
        if !self
            .wheel_file
            .to_ascii_lowercase()
            .starts_with(&wheel_prefix)
        {
            return Err(LayerError::Invalid(
                "wheel identity differs from approval".into(),
            ));
        }
        Ok(())
    }
}

impl WheelStageInstaller {
    pub fn destination(&self, approved: &ApprovedWheelStage) -> Result<PathBuf, LayerError> {
        approved.validate()?;
        if !self.data_directory.is_absolute() || self.timeout.is_zero() {
            return Err(LayerError::Invalid(
                "portable AIData and bounded timeout required".into(),
            ));
        }
        Ok(self
            .data_directory
            .join("build-stages")
            .join(&approved.plugin_id)
            .join(&approved.plugin_revision)
            .join(&approved.build_stage.id))
    }

    pub fn verify_inputs(&self, approved: &ApprovedWheelStage) -> Result<(), LayerError> {
        let destination = self.destination(approved)?;
        let wheel = self
            .data_directory
            .join("cache")
            .join("wheels")
            .join(&approved.wheel_sha256)
            .join(&approved.wheel_file);
        reject_symlink_chain(&wheel, &self.data_directory)?;
        reject_symlink_chain(&destination, &self.data_directory)?;
        if !sha256_file(&wheel)?.eq_ignore_ascii_case(&approved.wheel_sha256)
            || !sha256_file(&self.uv_executable)?.eq_ignore_ascii_case(&approved.uv_sha256)
            || !sha256_file(&self.python_executable)?.eq_ignore_ascii_case(&approved.python_sha256)
        {
            return Err(LayerError::Invalid(
                "wheel or toolchain checksum mismatch".into(),
            ));
        }
        Ok(())
    }

    pub async fn install(&self, approved: &ApprovedWheelStage) -> Result<PathBuf, LayerError> {
        self.verify_inputs(approved)?;
        let destination = self.destination(approved)?;
        let wheel = self
            .data_directory
            .join("cache")
            .join("wheels")
            .join(&approved.wheel_sha256)
            .join(&approved.wheel_file);
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(&self.python_executable)
                .arg("--version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| LayerError::Timeout)??;
        let version = String::from_utf8_lossy(&output.stdout);
        if !output.status.success()
            || version.trim() != format!("Python {}", approved.python_version)
        {
            return Err(LayerError::Invalid(
                "managed Python does not match approved version".into(),
            ));
        }
        let cache_directory = self.data_directory.join("cache/uv");
        LayerInstaller {
            uv_executable: &self.uv_executable,
            python_executable: &self.python_executable,
            cache_directory: &cache_directory,
            timeout: self.timeout,
        }
        .publish_local_wheel(
            &destination,
            ".neuromita-build-stage.json",
            approved,
            &approved.package,
            &wheel,
            &approved.wheel_sha256,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approved() -> ApprovedWheelStage {
        ApprovedWheelStage {
            plugin_id: "test_plugin".into(),
            plugin_revision: "rev1".into(),
            python_version: "3.13.5".into(),
            build_stage: BuildStage {
                id: "verified_wheel".into(),
                kind: BuildStageKind::PythonWheel,
                backend: "torch-cuda".into(),
                platform: host_platform(),
                requirement: Some("sample_pkg==1.2.3".into()),
            },
            package: LockedPackage {
                name: "sample_pkg".into(),
                version: "1.2.3".into(),
            },
            wheel_file: "sample_pkg-1.2.3-py3-none-any.whl".into(),
            wheel_sha256: "a".repeat(64),
            install_plan_sha256: "b".repeat(64),
            uv_sha256: "c".repeat(64),
            python_sha256: "d".repeat(64),
        }
    }

    #[test]
    fn exact_pins_required_and_jit_never_passes_for_wheel_executor() {
        let mut stage = approved();
        assert!(stage.validate().is_ok());
        stage.build_stage.kind = BuildStageKind::RuntimeJit;
        assert!(stage.validate().is_err());
        stage = approved();
        stage.build_stage.requirement = Some("sample_pkg>=1.2".into());
        assert!(stage.validate().is_err());
        stage = approved();
        stage.wheel_file = "../sample_pkg-1.2.3-py3-none-any.whl".into();
        assert!(stage.validate().is_err());
        stage = approved();
        stage.build_stage.platform = match host_platform() {
            BuildPlatform::Windows => BuildPlatform::Linux,
            _ => BuildPlatform::Windows,
        };
        assert!(stage.validate().is_err());
        stage = approved();
        stage.python_sha256 = "xyz".into();
        assert!(stage.validate().is_err());
    }

    #[test]
    fn wheel_inputs_are_confined_to_portable_data() {
        let root = std::env::temp_dir().join(format!("nm-wheel-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let executor = WheelStageInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            data_directory: root.clone(),
            timeout: Duration::from_secs(10),
        };
        let dest = executor.destination(&approved()).unwrap();
        assert!(dest.starts_with(root.join("build-stages")));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", root.join("build-stages")).unwrap();
            assert!(reject_symlink_chain(&dest, &root).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn actual_offline_wheel_publish_is_atomic_idempotent_and_checksum_bound() {
        let (Ok(uv), Ok(python)) = (
            std::env::var("NEUROMITA_C4_TEST_UV"),
            std::env::var("NEUROMITA_C4_TEST_PYTHON"),
        ) else {
            return;
        };
        let root =
            std::env::temp_dir().join(format!("nm-wheel-stage-real-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let wheel_name = "sample_pkg-1.2.3-py3-none-any.whl";
        let wheel = root.join(wheel_name);
        let script = r#"
import sys
from zipfile import ZipFile
path = sys.argv[1]
dist = 'sample_pkg-1.2.3.dist-info/'
with ZipFile(path, 'w') as z:
    z.writestr('sample_pkg/__init__.py', 'VALUE = 42\n')
    z.writestr(dist + 'METADATA', 'Metadata-Version: 2.1\nName: sample-pkg\nVersion: 1.2.3\n')
    z.writestr(dist + 'WHEEL', 'Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n')
    z.writestr(dist + 'RECORD', '')
"#;
        let output = std::process::Command::new(&python)
            .args(["-c", script, wheel.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);

        let mut stage = approved();
        stage.wheel_sha256 = sha256_file(&wheel).unwrap();
        stage.uv_sha256 = sha256_file(Path::new(&uv)).unwrap();
        stage.python_sha256 = sha256_file(Path::new(&python)).unwrap();
        let version = std::process::Command::new(&python)
            .arg("--version")
            .output()
            .unwrap();
        stage.python_version = String::from_utf8(version.stdout)
            .unwrap()
            .trim()
            .strip_prefix("Python ")
            .unwrap()
            .to_owned();
        let wheel_cache = root.join("cache/wheels").join(&stage.wheel_sha256);
        std::fs::create_dir_all(&wheel_cache).unwrap();
        std::fs::rename(&wheel, wheel_cache.join(wheel_name)).unwrap();
        let installer = WheelStageInstaller {
            uv_executable: uv.into(),
            python_executable: python.into(),
            data_directory: root.clone(),
            timeout: Duration::from_secs(30),
        };
        let path = installer.install(&stage).await.unwrap();
        assert!(path.join("sample_pkg/__init__.py").is_file());
        assert!(path.join(".neuromita-build-stage.json").is_file());
        assert!(!path.join(".verified-wheel-input").exists());
        assert_eq!(installer.install(&stage).await.unwrap(), path);
        std::fs::write(wheel_cache.join(wheel_name), b"corrupt").unwrap();
        assert!(installer.install(&stage).await.is_err());
        assert!(path.join("sample_pkg/__init__.py").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
