//! Read-only inventory of a bounded subset of files owned by the persistent application.
//! This is neither a backup nor a complete cross-owner restoration contract.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::timeline::storage::SqliteTimelineRepository;

const FORMAT: &str = "neuromita-recovery-inventory";
const VERSION: u32 = 1;
pub(crate) const MAX_JSON_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const CONFIG_FILES: [&str; 4] = [
    "settings.json",
    "presets.json",
    "preset-credentials.json",
    "finetune.json",
];
const UNVERIFIED_OWNERS: [&str; 4] = [
    "session_archive_and_external_history_assets",
    "external_ai_models_plugins_and_voice_assets",
    "external_game_and_user_documents",
    "cross_owner_atomic_restore_and_live_cutover",
];

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("invalid recovery data directory or unsafe file: {0}")]
    UnsafePath(String),
    #[error("recovery inventory input changed or does not match its manifest")]
    Changed,
    #[error("recovery inventory JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("recovery inventory file exceeds the bounded size limit")]
    TooLarge,
    #[error(
        "snapshot directory publication completed, but durability is uncertain at {destination}: {cause}"
    )]
    PublicationUncertain {
        destination: std::path::PathBuf,
        cause: String,
    },
    #[error("Timeline inspection failed: {0}")]
    Timeline(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TimelineEvidence {
    pub path: String,
    pub revision: u64,
    pub canonical_events: usize,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileEvidence {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryInventory {
    pub format: String,
    pub version: u32,
    pub scope: String,
    pub complete_dataset_recovery: bool,
    pub timeline: TimelineEvidence,
    pub config_files: Vec<FileEvidence>,
    pub missing_optional_config_files: Vec<String>,
    pub unverified_owners: Vec<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn ensure_directory(root: &Path) -> Result<(), InventoryError> {
    if !root.is_absolute() || !root.is_dir() {
        return Err(InventoryError::UnsafePath(root.display().to_string()));
    }
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(InventoryError::UnsafePath(root.display().to_string()));
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<fs::Metadata, InventoryError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(InventoryError::UnsafePath(path.display().to_string()));
    }
    Ok(metadata)
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

pub(crate) fn read_bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, InventoryError> {
    let before = ensure_regular_file(path)?;
    if before.len() > limit {
        return Err(InventoryError::TooLarge);
    }
    let file = File::open(path)?;
    if !same_file(&before, &file.metadata()?) {
        return Err(InventoryError::Changed);
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(InventoryError::TooLarge);
    }
    if !same_file(&before, &ensure_regular_file(path)?) {
        return Err(InventoryError::Changed);
    }
    Ok(bytes)
}

fn timeline_evidence(path: &Path) -> Result<TimelineEvidence, InventoryError> {
    ensure_regular_file(path)?;
    let verified = SqliteTimelineRepository::inspect_verified_snapshot(path)
        .map_err(|error| InventoryError::Timeline(error.to_string()))?;
    Ok(TimelineEvidence {
        path: "timeline.sqlite3".into(),
        revision: verified.revision,
        canonical_events: verified.canonical_events,
        fingerprint: verified.source_fingerprint,
    })
}

fn scan_config_files(root: &Path) -> Result<(Vec<FileEvidence>, Vec<String>), InventoryError> {
    let mut config_files = Vec::new();
    let mut missing_optional_config_files = Vec::new();
    for name in CONFIG_FILES {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let bytes = read_bounded_file(&path, MAX_JSON_BYTES)?;
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|error| InventoryError::InvalidJson(format!("{name}: {error}")))?;
                config_files.push(FileEvidence {
                    path: name.into(),
                    bytes: bytes.len() as u64,
                    sha256: sha256_hex(&bytes),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing_optional_config_files.push(name.into());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok((config_files, missing_optional_config_files))
}

/// Collect the operator-visible known persistent files without changing them.
/// Other owners are expressly excluded; the result is never a complete recovery manifest.
pub fn inspect(root: impl AsRef<Path>) -> Result<RecoveryInventory, InventoryError> {
    let root = root.as_ref();
    ensure_directory(root)?;
    let timeline_path = root.join("timeline.sqlite3");
    let first_timeline = timeline_evidence(&timeline_path)?;
    let (config_files, missing_optional_config_files) = scan_config_files(root)?;
    if first_timeline != timeline_evidence(&timeline_path)?
        || (config_files.clone(), missing_optional_config_files.clone()) != scan_config_files(root)?
    {
        return Err(InventoryError::Changed);
    }
    Ok(RecoveryInventory {
        format: FORMAT.into(),
        version: VERSION,
        scope: "known_persistent_local_files_only".into(),
        complete_dataset_recovery: false,
        timeline: first_timeline,
        config_files,
        missing_optional_config_files,
        unverified_owners: UNVERIFIED_OWNERS.iter().map(|s| (*s).into()).collect(),
    })
}

/// Verify a separately retained manifest against a *new* read-only inspection.
/// An internally matching manifest does not authenticate provenance or grant restore permission.
pub fn verify(
    root: impl AsRef<Path>,
    expected: &RecoveryInventory,
) -> Result<RecoveryInventory, InventoryError> {
    let actual = inspect(root)?;
    if expected != &actual {
        return Err(InventoryError::Changed);
    }
    Ok(actual)
}

pub fn parse_manifest(bytes: &[u8]) -> Result<RecoveryInventory, InventoryError> {
    if bytes.len() > 64 * 1024 {
        return Err(InventoryError::TooLarge);
    }
    let manifest: RecoveryInventory = serde_json::from_slice(bytes)
        .map_err(|error| InventoryError::InvalidJson(error.to_string()))?;
    if manifest.format != FORMAT
        || manifest.version != VERSION
        || manifest.scope != "known_persistent_local_files_only"
        || manifest.complete_dataset_recovery
        || manifest.unverified_owners
            != UNVERIFIED_OWNERS
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
    {
        return Err(InventoryError::InvalidJson(
            "unsupported manifest contract".into(),
        ));
    }
    Ok(manifest)
}

pub fn read_manifest(path: impl AsRef<Path>) -> Result<RecoveryInventory, InventoryError> {
    parse_manifest(&read_bounded_file(path.as_ref(), 64 * 1024)?)
}

pub fn json(manifest: &RecoveryInventory) -> Result<String, InventoryError> {
    serde_json::to_string_pretty(manifest)
        .map_err(|error| InventoryError::InvalidJson(error.to_string()))
}
