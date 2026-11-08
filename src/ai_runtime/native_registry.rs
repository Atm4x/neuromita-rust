//! Read-only discovery and exact-content admission for independent native model packages.
//! Package manifests witness available files; only a live worker can establish Ready.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::native_model::{NativeBackend, NativeModelLaunch};

const MANIFEST_NAME: &str = "native-model.json";
const MAX_MANIFEST_BYTES: u64 = 128 * 1024;
const MAX_PACKAGES: usize = 128;
const MAX_REFERENCES: usize = 128;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum NativeRegistryError {
    #[error("native package layout or manifest is invalid: {0}")]
    Invalid(&'static str),
    #[error("native package IO failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("native package manifest is malformed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("native package file differs from its declared SHA-256 or length")]
    Integrity,
    #[error("native package manifest changed after discovery")]
    ChangedManifest,
    #[error("native package manifest digest was not approved")]
    Unapproved,
    #[error("native package does not support the selected backend or platform")]
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFileRef {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRuntimeFile {
    pub backends: Vec<NativeBackend>,
    pub file: NativeFileRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelManifest {
    pub schema_version: u32,
    pub model_id: String,
    pub display_name: String,
    pub package_version: String,
    pub os: String,
    pub arch: String,
    pub protocol_version: u32,
    pub backends: Vec<NativeBackend>,
    pub executable: NativeFileRef,
    pub assets: Vec<NativeFileRef>,
    #[serde(default)]
    pub runtime_files: Vec<NativeRuntimeFile>,
}

#[derive(Debug, Clone)]
pub struct NativeModelRegistration {
    package_directory: PathBuf,
    manifest_sha256: [u8; 32],
    manifest: NativeModelManifest,
}

impl NativeModelRegistration {
    pub fn manifest(&self) -> &NativeModelManifest {
        &self.manifest
    }

    pub fn package_directory(&self) -> &Path {
        &self.package_directory
    }

    pub fn manifest_sha256(&self) -> [u8; 32] {
        self.manifest_sha256
    }

    pub fn prepare_launch(
        &self,
        approved_manifest_sha256: [u8; 32],
        backend: NativeBackend,
        runtime_directory: &Path,
        request_timeout: Duration,
    ) -> Result<NativeModelLaunch, NativeRegistryError> {
        if approved_manifest_sha256 != self.manifest_sha256 {
            return Err(NativeRegistryError::Unapproved);
        }
        let path = self.package_directory.join(MANIFEST_NAME);
        check_path(&path, false)?;
        let bytes = read_manifest(&path)?;
        if sha256(&bytes) != self.manifest_sha256 {
            return Err(NativeRegistryError::ChangedManifest);
        }
        if self.manifest.os != std::env::consts::OS
            || self.manifest.arch != std::env::consts::ARCH
            || !self.manifest.backends.contains(&backend)
        {
            return Err(NativeRegistryError::Incompatible);
        }
        check_path(runtime_directory, true)?;
        verify_file(&self.package_directory, &self.manifest.executable)?;
        for asset in &self.manifest.assets {
            verify_file(&self.package_directory, asset)?;
        }
        for runtime in &self.manifest.runtime_files {
            if runtime.backends.contains(&backend) {
                verify_file(runtime_directory, &runtime.file)?;
            }
        }
        if request_timeout.is_zero() {
            return Err(NativeRegistryError::Invalid("zero request timeout"));
        }
        Ok(NativeModelLaunch {
            model_id: self.manifest.model_id.clone(),
            executable: self.package_directory.join(&self.manifest.executable.path),
            executable_sha256: parse_sha256(&self.manifest.executable.sha256)?,
            model_directory: self.package_directory.clone(),
            runtime_directory: runtime_directory.to_path_buf(),
            backend,
            request_timeout,
        })
    }
}

pub struct NativeModelRegistry;

impl NativeModelRegistry {
    pub fn discover(root: &Path) -> Result<Vec<NativeModelRegistration>, NativeRegistryError> {
        check_path(root, true)?;
        let mut result = Vec::new();
        for entry in fs::read_dir(root)? {
            if result.len() >= MAX_PACKAGES {
                return Err(NativeRegistryError::Invalid(
                    "too many native model packages",
                ));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(NativeRegistryError::Invalid("symlinked package entry"));
            }
            if !kind.is_dir() {
                return Err(NativeRegistryError::Invalid("unexpected registry entry"));
            }
            let directory = entry.path();
            check_path(&directory, true)?;
            let path = directory.join(MANIFEST_NAME);
            check_path(&path, false)?;
            let bytes = read_manifest(&path)?;
            let manifest: NativeModelManifest = serde_json::from_slice(&bytes)?;
            validate_manifest(&manifest)?;
            if directory.file_name().and_then(|s| s.to_str()) != Some(manifest.model_id.as_str()) {
                return Err(NativeRegistryError::Invalid(
                    "model directory and id differ",
                ));
            }
            result.push(NativeModelRegistration {
                package_directory: directory,
                manifest_sha256: sha256(&bytes),
                manifest,
            });
        }
        result.sort_by(|a, b| a.manifest.model_id.cmp(&b.manifest.model_id));
        Ok(result)
    }
}

fn check_path(path: &Path, directory: bool) -> Result<(), NativeRegistryError> {
    if !path.is_absolute() {
        return Err(NativeRegistryError::Invalid(
            "managed paths must be absolute",
        ));
    }
    for component in path.ancestors() {
        let metadata = fs::symlink_metadata(component)?;
        if metadata.file_type().is_symlink() {
            return Err(NativeRegistryError::Invalid("symlink in managed path"));
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(NativeRegistryError::Invalid("unexpected file type"));
    }
    Ok(())
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').count() <= 12
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.len() <= 96
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        })
}

fn parse_sha256(value: &str) -> Result<[u8; 32], NativeRegistryError> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(NativeRegistryError::Invalid("invalid sha256 encoding"));
    }
    let mut digest = [0; 32];
    for (i, value) in digest.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&bytes[i * 2..i * 2 + 2])
            .map_err(|_| NativeRegistryError::Invalid("invalid digest"))?;
        *value = u8::from_str_radix(pair, 16)
            .map_err(|_| NativeRegistryError::Invalid("invalid digest"))?;
    }
    Ok(digest)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

fn validate_manifest(manifest: &NativeModelManifest) -> Result<(), NativeRegistryError> {
    if manifest.schema_version != 1 || manifest.protocol_version != 1 {
        return Err(NativeRegistryError::Invalid(
            "unsupported manifest/protocol version",
        ));
    }
    if !valid_identifier(&manifest.model_id)
        || !valid_version(&manifest.package_version)
        || manifest.display_name.is_empty()
        || manifest.display_name.len() > 128
        || manifest.display_name.chars().any(char::is_control)
        || !valid_identifier(&manifest.os)
        || !valid_identifier(&manifest.arch)
    {
        return Err(NativeRegistryError::Invalid("model metadata or platform"));
    }
    if manifest.backends.is_empty() || manifest.backends.len() > 4 {
        return Err(NativeRegistryError::Invalid("invalid backends"));
    }
    let mut backends = HashSet::new();
    for backend in &manifest.backends {
        if !backends.insert(backend.as_str()) {
            return Err(NativeRegistryError::Invalid("duplicate backend"));
        }
    }
    if manifest.executable.size_bytes > 512 * 1024 * 1024 {
        return Err(NativeRegistryError::Invalid("native executable too large"));
    }
    if manifest.assets.is_empty()
        || manifest.assets.len() + manifest.runtime_files.len() + 1 > MAX_REFERENCES
    {
        return Err(NativeRegistryError::Invalid("invalid native asset count"));
    }
    let mut files = HashSet::new();
    for file in std::iter::once(&manifest.executable)
        .chain(manifest.assets.iter())
        .chain(manifest.runtime_files.iter().map(|runtime| &runtime.file))
    {
        if !safe_relative(&file.path) || file.size_bytes == 0 || file.size_bytes > MAX_FILE_BYTES {
            return Err(NativeRegistryError::Invalid(
                "invalid native file reference",
            ));
        }
        parse_sha256(&file.sha256)?;
    }
    for file in std::iter::once(&manifest.executable).chain(manifest.assets.iter()) {
        if !files.insert(file.path.to_ascii_lowercase()) {
            return Err(NativeRegistryError::Invalid(
                "duplicate package file reference",
            ));
        }
    }
    files.clear();
    for runtime in &manifest.runtime_files {
        if runtime.backends.is_empty() || runtime.backends.len() > manifest.backends.len() {
            return Err(NativeRegistryError::Invalid("runtime backend selection"));
        }
        let mut selected = HashSet::new();
        for backend in &runtime.backends {
            if !manifest.backends.contains(backend) || !selected.insert(backend.as_str()) {
                return Err(NativeRegistryError::Invalid("runtime backend selection"));
            }
        }
        if !files.insert(runtime.file.path.to_ascii_lowercase()) {
            return Err(NativeRegistryError::Invalid(
                "duplicate runtime file reference",
            ));
        }
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<Vec<u8>, NativeRegistryError> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 || len > MAX_MANIFEST_BYTES {
        return Err(NativeRegistryError::Invalid("native manifest size"));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != len {
        return Err(NativeRegistryError::Invalid(
            "native manifest changed during read",
        ));
    }
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut output = [0; 32];
    output.copy_from_slice(digest.as_ref());
    output
}

fn verify_file(root: &Path, file: &NativeFileRef) -> Result<(), NativeRegistryError> {
    let path = root.join(&file.path);
    check_path(&path, false)?;
    let input = File::open(&path)?;
    if input.metadata()?.len() != file.size_bytes {
        return Err(NativeRegistryError::Integrity);
    }
    let expected = parse_sha256(&file.sha256)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0; 65536];
    let mut read_total = 0_u64;
    let mut input = input;
    loop {
        let size = input.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        read_total += size as u64;
        if read_total > file.size_bytes {
            return Err(NativeRegistryError::Integrity);
        }
        digest.update(&buffer[..size]);
    }
    if read_total != file.size_bytes || digest.finish().as_ref() != expected {
        return Err(NativeRegistryError::Integrity);
    }
    Ok(())
}
