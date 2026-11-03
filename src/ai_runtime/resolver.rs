//! Resolve plugin package pins through uv without mutating active environments.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use tokio::{process::Command, time::timeout};
use uuid::Uuid;
use super::{layer::validate_lock, overlay::LockedPackage};

#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("invalid resolution request: {0}")]
    Invalid(String),
    #[error("dependency conflict: {0}")]
    Conflict(String),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv resolution timed out")]
    Timeout,
    #[error("uv dependency resolution failed: {0}")]
    Uv(String),
    #[error("unsupported uv lock syntax: {0}")]
    Unsupported(String),
}

pub struct DependencyResolver {
    pub uv_executable: PathBuf,
    pub python_executable: PathBuf,
    pub cache_directory: PathBuf,
    pub timeout: Duration,
}

impl DependencyResolver {
    pub async fn resolve(
        &self,
        direct: &[LockedPackage],
        constraints: &[LockedPackage],
        python_version: &str,
    ) -> Result<Vec<LockedPackage>, ResolveError> {
        if !self.uv_executable.is_absolute() || !self.python_executable.is_absolute()
            || !self.cache_directory.is_absolute() || self.timeout.is_zero()
        {
            return Err(ResolveError::Invalid("absolute paths and nonzero timeout required".into()));
        }
        validate_lock(direct, python_version).map_err(|e| ResolveError::Invalid(e.to_string()))?;
        validate_lock(constraints, python_version).map_err(|e| ResolveError::Invalid(e.to_string()))?;
        if direct.is_empty() { return Ok(Vec::new()); }
        let mut backend = HashMap::new();
        for package in constraints {
            backend.insert(normalize(&package.name), package.version.as_str());
        }
        for package in direct {
            if let Some(version) = backend.get(&normalize(&package.name))
                && *version != package.version
            {
                return Err(ResolveError::Conflict(format!(
                    "{}: backend {version}, plugin {}", package.name, package.version
                )));
            }
        }
        if !self.uv_executable.is_file() || !self.python_executable.is_file() {
            return Err(ResolveError::Invalid("managed uv or Python missing".into()));
        }
        let root = self.cache_directory.join("resolution");
        tokio::fs::create_dir_all(&root).await?;
        let stage = root.join(format!(".staging-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&stage).await?;
        let result = self.compile_in(&stage, direct, constraints).await;
        let _ = tokio::fs::remove_dir_all(stage).await;
        let packages = result?;
        for package in &packages {
            if let Some(version) = backend.get(&normalize(&package.name))
                && package.version != *version
            {
                return Err(ResolveError::Conflict(format!(
                    "{}: backend {version}, resolved {}", package.name, package.version
                )));
            }
        }
        Ok(packages.into_iter().filter(|package| {
            !backend.contains_key(&normalize(&package.name))
        }).collect())
    }

    async fn compile_in(
        &self,
        stage: &Path,
        direct: &[LockedPackage],
        constraints: &[LockedPackage],
    ) -> Result<Vec<LockedPackage>, ResolveError> {
        let input = stage.join("requirements.in");
        let output = stage.join("requirements.lock");
        let bounds = stage.join("backend-constraints.txt");
        tokio::fs::write(&input, serialize_pins(direct)).await?;
        tokio::fs::write(&bounds, serialize_pins(constraints)).await?;
        let mut command = Command::new(&self.uv_executable);
        command.args(["pip", "compile", "--no-config", "--no-python-downloads", "--python"])
            .arg(&self.python_executable)
            .arg("--output-file").arg(&output)
            .arg("--constraint").arg(&bounds)
            .arg("--no-build")
            .arg(&input)
            .env("UV_CACHE_DIR", &self.cache_directory)
            .env_remove("VIRTUAL_ENV")
            .env_remove("UV_PROJECT_ENVIRONMENT")
            .current_dir(stage)
            .kill_on_drop(true);
        let response = timeout(self.timeout, command.output())
            .await.map_err(|_| ResolveError::Timeout)??;
        if !response.status.success() {
            return Err(ResolveError::Uv(
                String::from_utf8_lossy(&response.stderr).chars().take(1600).collect()
            ));
        }
        let packages = parse_lock(&tokio::fs::read_to_string(output).await?)?;
        let resolved: HashMap<_,_> = packages.iter()
            .map(|package| (normalize(&package.name), package.version.as_str()))
            .collect();
        for direct_pin in direct {
            if resolved.get(&normalize(&direct_pin.name)).copied()
                != Some(direct_pin.version.as_str())
            {
                return Err(ResolveError::Unsupported(format!(
                    "missing direct pin {}=={}", direct_pin.name, direct_pin.version
                )));
            }
        }
        Ok(packages)
    }
}

fn normalize(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', '_', '.'], "-")
}

fn serialize_pins(pins: &[LockedPackage]) -> String {
    pins.iter().map(|package| {
        format!("{}=={}\n", package.name, package.version)
    }).collect()
}

fn parse_lock(source: &str) -> Result<Vec<LockedPackage>, ResolveError> {
    let mut found = HashSet::new();
    let mut pins = Vec::new();
    for line in source.lines() {
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') { continue; }
        if text.starts_with('-') || text.starts_with('\\')
            || text.contains(';') || text.contains('@')
        {
            return Err(ResolveError::Unsupported(text.into()));
        }
        let (name, version) = text.split_once("==")
            .ok_or_else(|| ResolveError::Unsupported(text.into()))?;
        if name.is_empty() || version.is_empty()
            || version.contains('=') || version.contains(char::is_whitespace)
        {
            return Err(ResolveError::Unsupported(text.into()));
        }
        let pin = LockedPackage {name:name.into(), version:version.into()};
        validate_lock(std::slice::from_ref(&pin), "3.13.5")
            .map_err(|_| ResolveError::Unsupported(text.into()))?;
        if !found.insert(normalize(&pin.name)) {
            return Err(ResolveError::Unsupported(format!("duplicate {name}")));
        }
        pins.push(pin);
    }
    if pins.len() > 256 {
        return Err(ResolveError::Unsupported("too many packages".into()));
    }
    Ok(pins)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_simple_exact_locks_are_accepted() {
        let pins = parse_lock("# uv\nhttpx==0.28.1\n  # via package\nanyio==4.9.0\n").unwrap();
        assert_eq!(pins.len(), 2);
        assert!(parse_lock("httpx==1; python_version>='3.11'").is_err());
        assert!(parse_lock("--index-url https://host.test").is_err());
        assert!(parse_lock("httpx==1\nhttpx==2\n").is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn uv_compiles_transitive_dependencies_without_backend_duplication() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("nm-resolver-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let uv = root.join("uv");
        tokio::fs::write(&uv,
            "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do if [ \"$1\" = --output-file ]; then shift; out=$1; fi; shift; done\nprintf 'torch==2.8.0\\ntransformers==4.45.0\\ntokenizers==0.21.0\\n' > \"$out\"\n"
        ).await.unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let python = root.join("python");
        tokio::fs::write(&python, "fake").await.unwrap();
        let resolver = DependencyResolver {
            uv_executable:uv, python_executable:python,
            cache_directory:root.join("cache"), timeout:Duration::from_secs(4),
        };
        let packages = resolver.resolve(
            &[LockedPackage {name:"transformers".into(),version:"4.45.0".into()}],
            &[LockedPackage {name:"torch".into(),version:"2.8.0".into()}],
            "3.13.5",
        ).await.unwrap();
        assert_eq!(packages.len(), 2);
        assert!(packages.iter().any(|package| package.name == "tokenizers"));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
