//! Trusted, executable-embedded Python SDK: immutable revisions and atomic active selection.
//! The file bytes are pinned by the Rust executable rather than copied from source at runtime.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use ring::digest::{Context, SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const SDK_API_VERSION: u32 = 1;

const FILES: &[(&str, &[u8])] = &[
    (
        "__init__.py",
        include_bytes!("../../python/neuromita_ai_sdk/__init__.py"),
    ),
    (
        "assets.py",
        include_bytes!("../../python/neuromita_ai_sdk/assets.py"),
    ),
    (
        "catalog.py",
        include_bytes!("../../python/neuromita_ai_sdk/catalog.py"),
    ),
    (
        "constraints.py",
        include_bytes!("../../python/neuromita_ai_sdk/constraints.py"),
    ),
    (
        "distributions.py",
        include_bytes!("../../python/neuromita_ai_sdk/distributions.py"),
    ),
    (
        "hardware.py",
        include_bytes!("../../python/neuromita_ai_sdk/hardware.py"),
    ),
    (
        "host.py",
        include_bytes!("../../python/neuromita_ai_sdk/host.py"),
    ),
    (
        "installer.py",
        include_bytes!("../../python/neuromita_ai_sdk/installer.py"),
    ),
    (
        "settings.py",
        include_bytes!("../../python/neuromita_ai_sdk/settings.py"),
    ),
];

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("SDK root must be absolute and cannot contain symlinks")]
    UnsafeRoot,
    #[error("installed SDK is absent")]
    Missing,
    #[error("installed SDK revision, receipt or files are corrupted")]
    Corrupt,
    #[error("installed SDK revision differs from this executable")]
    Incompatible,
    #[error("SDK filesystem failure: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Receipt {
    api_version: u32,
    revision: String,
    files: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Active {
    api_version: u32,
    revision: String,
}

fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn receipt(files: &[(&str, &[u8])]) -> Receipt {
    let mut hash = Context::new(&SHA256);
    let mut entries = Vec::with_capacity(files.len());
    for (name, bytes) in files {
        hash.update(&(*name).len().to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&bytes.len().to_le_bytes());
        hash.update(bytes);
        entries.push(((*name).to_owned(), sha(bytes)));
    }
    Receipt {
        api_version: SDK_API_VERSION,
        revision: format!("sha256-{}", sha(hash.finish().as_ref())),
        files: entries,
    }
}

fn clean_directory(path: &Path) -> Result<(), SdkError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SdkError::UnsafeRoot);
        }
    }
    Ok(())
}

fn clean_file(path: &Path) -> Result<Vec<u8>, SdkError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SdkError::Corrupt)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(SdkError::Corrupt);
    }
    fs::read(path).map_err(|_| SdkError::Corrupt)
}

fn sync_write(path: &Path, bytes: &[u8]) -> Result<(), SdkError> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// A portable AIData-scoped SDK publisher. Existing subprocesses continue using
/// their immutable revision when active.json switches to a new one.
#[derive(Debug, Clone)]
pub struct SdkPublisher {
    pub ai_data: PathBuf,
}

impl SdkPublisher {
    pub fn new(ai_data: PathBuf) -> Self {
        Self { ai_data }
    }

    pub fn sdk_directory(&self) -> PathBuf {
        self.ai_data.join("sdk")
    }

    fn check_root(&self) -> Result<(), SdkError> {
        if !self.ai_data.is_absolute() {
            return Err(SdkError::UnsafeRoot);
        }
        let sdk = self.sdk_directory();
        for path in [&self.ai_data, &sdk, &sdk.join("releases")] {
            clean_directory(path)?;
        }
        Ok(())
    }

    fn verify_revision(&self, rev: &str, expected: Option<&Receipt>) -> Result<PathBuf, SdkError> {
        if !rev.starts_with("sha256-")
            || rev.len() != 71
            || !rev[7..].bytes().all(|x| x.is_ascii_hexdigit())
        {
            return Err(SdkError::Corrupt);
        }
        let release = self.sdk_directory().join("releases").join(rev);
        clean_directory(&release).map_err(|_| SdkError::Corrupt)?;
        clean_directory(&release.join("neuromita_ai_sdk")).map_err(|_| SdkError::Corrupt)?;
        let bytes = clean_file(&release.join("receipt.json"))?;
        let marker: Receipt = serde_json::from_slice(&bytes).map_err(|_| SdkError::Corrupt)?;
        if marker.api_version != SDK_API_VERSION
            || marker.revision != rev
            || marker.files.len() != FILES.len()
        {
            return Err(SdkError::Corrupt);
        }
        if let Some(expected) = expected
            && marker != *expected
        {
            return Err(SdkError::Corrupt);
        }
        let mut parts: Vec<(&str, Vec<u8>)> = Vec::new();
        for ((name, hash), (known_name, _)) in marker.files.iter().zip(FILES.iter()) {
            if name != known_name {
                return Err(SdkError::Corrupt);
            }
            let path = release.join("neuromita_ai_sdk").join(name);
            let data = clean_file(&path)?;
            if sha(&data) != *hash {
                return Err(SdkError::Corrupt);
            }
            parts.push((name.as_str(), data));
        }
        let refs: Vec<(&str, &[u8])> = parts
            .iter()
            .map(|(name, data)| (*name, data.as_slice()))
            .collect();
        if receipt(&refs) != marker {
            return Err(SdkError::Corrupt);
        }
        Ok(release)
    }

    fn current(&self) -> Result<Active, SdkError> {
        let file = self.sdk_directory().join("active.json");
        let bytes = match clean_file(&file) {
            Ok(bytes) => bytes,
            Err(SdkError::Corrupt) if !file.exists() => return Err(SdkError::Missing),
            Err(error) => return Err(error),
        };
        serde_json::from_slice(&bytes).map_err(|_| SdkError::Corrupt)
    }

    /// Resolve only the revision compiled into this Rust executable. Never
    /// silently load an old, incomplete, source-tree or untrusted SDK.
    pub fn resolve_active(&self) -> Result<PathBuf, SdkError> {
        self.check_root()?;
        let active = self.current()?;
        let expected = receipt(FILES);
        if active.api_version != SDK_API_VERSION || active.revision != expected.revision {
            return Err(SdkError::Incompatible);
        }
        self.verify_revision(&active.revision, Some(&expected))
    }

    /// Publish trusted executable-embedded SDK bytes, leaving the previous
    /// active selection unchanged until the new immutable revision is valid.
    pub fn publish_embedded(&self) -> Result<PathBuf, SdkError> {
        self.publish(FILES)
    }

    fn publish(&self, files: &[(&str, &[u8])]) -> Result<PathBuf, SdkError> {
        self.check_root()?;
        let sdk = self.sdk_directory();
        fs::create_dir_all(sdk.join("releases"))?;
        let lock_path = sdk.join(".neuromita-sdk-publish.lock");
        if let Ok(metadata) = fs::symlink_metadata(&lock_path)
            && (!metadata.is_file() || metadata.len() != 0 || metadata.file_type().is_symlink())
        {
            return Err(SdkError::UnsafeRoot);
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        let metadata = fs::symlink_metadata(&lock_path)?;
        if !metadata.is_file() || metadata.len() != 0 || metadata.file_type().is_symlink() {
            return Err(SdkError::UnsafeRoot);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = lock.metadata()?;
            if held.dev() != metadata.dev() || held.ino() != metadata.ino() {
                return Err(SdkError::UnsafeRoot);
            }
        }
        lock.lock()?;
        let marker = receipt(files);
        match self.current() {
            Ok(previous) => {
                self.verify_revision(&previous.revision, None)?;
            }
            Err(SdkError::Missing) => (),
            Err(err) => return Err(err),
        }
        let release = sdk.join("releases").join(&marker.revision);
        if release.exists() {
            self.verify_revision(&marker.revision, Some(&marker))?;
        } else {
            let staging = sdk
                .join("releases")
                .join(format!(".staging-{}", Uuid::new_v4()));
            fs::create_dir(&staging)?;
            let result = (|| -> Result<(), SdkError> {
                fs::create_dir(staging.join("neuromita_ai_sdk"))?;
                for (name, contents) in files {
                    if !FILES.iter().any(|(allowed, _)| allowed == name) {
                        return Err(SdkError::Corrupt);
                    }
                    sync_write(&staging.join("neuromita_ai_sdk").join(name), contents)?;
                }
                sync_write(
                    &staging.join("receipt.json"),
                    &serde_json::to_vec(&marker).map_err(|_| SdkError::Corrupt)?,
                )?;
                fs::rename(&staging, &release)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&staging);
            }
            result?;
            self.verify_revision(&marker.revision, Some(&marker))?;
        }
        let active = Active {
            api_version: SDK_API_VERSION,
            revision: marker.revision.clone(),
        };
        let temporary = sdk.join(format!(".active-{}.json", Uuid::new_v4()));
        sync_write(
            &temporary,
            &serde_json::to_vec(&active).map_err(|_| SdkError::Corrupt)?,
        )?;
        if let Err(error) = fs::rename(&temporary, sdk.join("active.json")) {
            let _ = fs::remove_file(&temporary);
            return Err(SdkError::Io(error));
        }
        self.resolve_active_with(&active, &marker)
    }

    fn resolve_active_with(&self, active: &Active, marker: &Receipt) -> Result<PathBuf, SdkError> {
        let saved = self.current()?;
        if saved != *active {
            return Err(SdkError::Corrupt);
        }
        self.verify_revision(&active.revision, Some(marker))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publisher() -> (SdkPublisher, PathBuf) {
        let root = std::env::temp_dir().join(format!("nm-sdk-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        (SdkPublisher::new(root.join("AIData")), root)
    }

    #[test]
    fn publishes_pinned_sdk_and_reuses_revision() {
        let (publisher, root) = publisher();
        let first = publisher.publish_embedded().unwrap();
        assert_eq!(publisher.resolve_active().unwrap(), first);
        assert_eq!(publisher.publish_embedded().unwrap(), first);
        assert!(first.join("neuromita_ai_sdk/host.py").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_corrupt_sdk_without_source_fallback_or_repair() {
        let (publisher, root) = publisher();
        assert!(matches!(publisher.resolve_active(), Err(SdkError::Missing)));
        let installed = publisher.publish_embedded().unwrap();
        fs::write(installed.join("neuromita_ai_sdk/host.py"), b"malicious").unwrap();
        assert!(matches!(publisher.resolve_active(), Err(SdkError::Corrupt)));
        assert!(matches!(
            publisher.publish_embedded(),
            Err(SdkError::Corrupt)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_active_receipt_prevents_upgrade_and_retains_selector() {
        let (publisher, root) = publisher();
        let original = publisher.publish_embedded().unwrap();
        let active = fs::read(publisher.sdk_directory().join("active.json")).unwrap();
        fs::write(original.join("receipt.json"), b"{}").unwrap();
        let mut alternative = FILES.to_vec();
        alternative[0] = ("__init__.py", b"alternate package");
        assert!(matches!(
            publisher.publish(&alternative),
            Err(SdkError::Corrupt)
        ));
        assert_eq!(
            fs::read(publisher.sdk_directory().join("active.json")).unwrap(),
            active
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn immutable_upgrade_changes_only_atomic_selector_and_retains_previous_revision() {
        let (publisher, root) = publisher();
        let original = publisher.publish_embedded().unwrap();
        let active_before = fs::read(publisher.sdk_directory().join("active.json")).unwrap();
        let poison = publisher
            .sdk_directory()
            .join("releases")
            .join(".staging-interrupted");
        fs::create_dir(&poison).unwrap();
        let mut alternative = FILES.to_vec();
        alternative[0] = ("__init__.py", b"different but pinned test SDK");
        publisher.publish(&alternative).unwrap();
        assert_eq!(
            fs::read(publisher.sdk_directory().join("active.json")).unwrap(),
            serde_json::to_vec(&Active {
                api_version: SDK_API_VERSION,
                revision: receipt(&alternative).revision
            })
            .unwrap()
        );
        assert!(original.join("neuromita_ai_sdk/host.py").exists());
        assert!(poison.is_dir());
        let incompatible = publisher.resolve_active();
        assert!(matches!(incompatible, Err(SdkError::Incompatible)));
        publisher.publish_embedded().unwrap();
        assert_eq!(
            fs::read(publisher.sdk_directory().join("active.json")).unwrap(),
            active_before
        );
        fs::remove_dir_all(root).unwrap();
    }
}
