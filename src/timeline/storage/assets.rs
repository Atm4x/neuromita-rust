//! Content-addressed binary attachments for canonical Timeline messages.
//! The directory is injected by the application composition root; this module
//! never chooses an installation path or owns message/branch visibility.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use ring::digest::{Context, SHA256};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::identity::EventId;

use super::super::{
    domain::{
        BranchId, EventDraft, EventKind, TimelineError, TimelineFact, TimelineRepository,
        canonical_message_content, parse_message_content,
    },
    view::MitaView,
};

const CHUNK: usize = 64 * 1024;

pub use super::super::domain::TimelineAssetRef;

#[derive(Debug, Error)]
pub enum TimelineAssetError {
    #[error("invalid Timeline asset reference or media type")]
    InvalidReference,
    #[error("Timeline asset root must be an absolute path")]
    InvalidRoot,
    #[error("Timeline asset exceeds the configured byte limit")]
    TooLarge,
    #[error("Timeline asset is missing")]
    Missing,
    #[error("Timeline asset integrity check failed")]
    Corrupt,
    #[error("attachment is not present in the current visible message")]
    NotVisible,
    #[error("Timeline asset I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum TimelineAttachmentCommitError {
    #[error(transparent)]
    Asset(#[from] TimelineAssetError),
    #[error(transparent)]
    Timeline(#[from] TimelineError),
}

/// Owns only binary bytes. Its root must be provided by the caller, not inferred
/// from the working directory, user profile or environment variables.
#[derive(Debug, Clone)]
pub struct TimelineAssetStore {
    directory: PathBuf,
    max_bytes: u64,
}

/// On-disk object without a reference in the supplied retention manifest.
/// Its MIME type cannot be inferred from the digest; it is not verified content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanAssetCandidate {
    pub digest: String,
    pub size: u64,
}

/// Read-only observation, not a GC authorization or a stable filesystem snapshot.
/// The caller is responsible for accounting for every external retained archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineAssetInventory {
    pub retained: Vec<TimelineAssetRef>,
    pub orphan_candidates: Vec<OrphanAssetCandidate>,
}

struct StagedFile {
    path: PathBuf,
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl TimelineAssetRef {
    pub fn validate(&self) -> Result<(), TimelineAssetError> {
        if self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !valid_media_type(&self.media_type)
            || self.size == 0
        {
            return Err(TimelineAssetError::InvalidReference);
        }
        Ok(())
    }
}

fn valid_media_type(media_type: &str) -> bool {
    let Some((major, minor)) = media_type.split_once('/') else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && media_type.len() <= 127
        && major.bytes().chain(minor.bytes()).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.')
        })
}

fn real_directory(path: &Path) -> Result<(), TimelineAssetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(TimelineAssetError::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(TimelineAssetError::InvalidReference);
    }
    Ok(())
}

fn real_regular_file(path: &Path) -> Result<(), TimelineAssetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(TimelineAssetError::Missing);
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(TimelineAssetError::Corrupt);
    }
    Ok(())
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn digest_hex(digest: ring::digest::Digest) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(64);
    for byte in digest.as_ref() {
        write!(&mut hex, "{byte:02x}").expect("String writes cannot fail");
    }
    hex
}

impl TimelineAssetStore {
    pub fn new(directory: impl AsRef<Path>, max_bytes: u64) -> Result<Self, TimelineAssetError> {
        if max_bytes == 0 {
            return Err(TimelineAssetError::TooLarge);
        }
        let directory = directory.as_ref().to_owned();
        if !directory.is_absolute() {
            return Err(TimelineAssetError::InvalidRoot);
        }
        fs::create_dir_all(&directory)?;
        real_directory(&directory)?;
        Ok(Self {
            directory,
            max_bytes,
        })
    }

    /// Stream a bounded upload into a temporary file, then publish with a
    /// no-replace hard link. Concurrent identical uploads converge to one blob.
    /// A caller must append its reference into Timeline separately; failed
    /// appends can leave unreferenced blobs, never fake canonical references.
    pub fn import(
        &self,
        media_type: &str,
        mut source: impl Read,
    ) -> Result<TimelineAssetRef, TimelineAssetError> {
        if !valid_media_type(media_type) {
            return Err(TimelineAssetError::InvalidReference);
        }
        real_directory(&self.directory)?;
        let staging = self.directory.join("staging");
        fs::create_dir_all(&staging)?;
        real_directory(&staging)?;
        let staged = StagedFile {
            path: staging.join(format!("{}.tmp", Uuid::new_v4())),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged.path)?;
        let mut hasher = Context::new(&SHA256);
        hasher.update(media_type.as_bytes());
        hasher.update(&[0]);
        let mut size = 0_u64;
        let mut buffer = [0_u8; CHUNK];
        loop {
            let length = source.read(&mut buffer)?;
            if length == 0 {
                break;
            }
            size = size
                .checked_add(length as u64)
                .ok_or(TimelineAssetError::TooLarge)?;
            if size > self.max_bytes {
                return Err(TimelineAssetError::TooLarge);
            }
            hasher.update(&buffer[..length]);
            file.write_all(&buffer[..length])?;
        }
        if size == 0 {
            return Err(TimelineAssetError::InvalidReference);
        }
        file.sync_all()?;
        drop(file);
        let reference = TimelineAssetRef {
            digest: digest_hex(hasher.finish()),
            size,
            media_type: media_type.to_owned(),
        };
        let destination = self.path_for(&reference)?;
        let objects = self.directory.join("objects");
        let shard = destination.parent().expect("asset has parent");
        fs::create_dir_all(&objects)?;
        real_directory(&objects)?;
        fs::create_dir_all(shard)?;
        self.check_object_directories(&destination)?;
        // A new objects/ or shard/ directory also needs its parent entry
        // persisted before a canonical Timeline event can reference the blob.
        // Repeat the sync even for concurrent duplicate imports: another
        // writer may have linked the file but not yet synced the directory.
        #[cfg(unix)]
        {
            File::open(&self.directory)?.sync_all()?;
            File::open(&objects)?.sync_all()?;
        }
        match fs::hard_link(&staged.path, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify(&reference)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        File::open(shard)?.sync_all()?;
        Ok(reference)
    }

    /// Inventory complete local object files against caller-supplied canonical
    /// retention roots. No data is removed. A malformed filesystem layout,
    /// symlink, or missing/corrupt retained blob fails the entire observation.
    /// Concurrent imports/appends or external archives not supplied by the
    /// caller may make an apparent orphan reachable; this cannot authorize GC.
    pub fn inventory(
        &self,
        retained: &[TimelineAssetRef],
    ) -> Result<TimelineAssetInventory, TimelineAssetError> {
        let mut roots = BTreeMap::new();
        for reference in retained {
            reference.validate()?;
            if reference.size > self.max_bytes {
                return Err(TimelineAssetError::TooLarge);
            }
            if roots
                .insert(reference.digest.clone(), reference.clone())
                .is_some_and(|previous| previous != *reference)
            {
                return Err(TimelineAssetError::InvalidReference);
            }
        }

        real_directory(&self.directory)?;
        let objects = self.directory.join("objects");
        let mut on_disk = BTreeMap::new();
        match fs::symlink_metadata(&objects) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !roots.is_empty() {
                    return Err(TimelineAssetError::Missing);
                }
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {
                real_directory(&objects)?;
                for shard in fs::read_dir(&objects)? {
                    let shard = shard?;
                    let name = shard.file_name();
                    let name = name.to_str().ok_or(TimelineAssetError::InvalidReference)?;
                    if name.len() != 2 || !is_lower_hex(name) {
                        return Err(TimelineAssetError::InvalidReference);
                    }
                    real_directory(&shard.path())?;
                    for entry in fs::read_dir(shard.path())? {
                        let entry = entry?;
                        let digest = entry.file_name();
                        let digest = digest
                            .to_str()
                            .ok_or(TimelineAssetError::InvalidReference)?;
                        if digest.len() != 64 || !digest.starts_with(name) || !is_lower_hex(digest)
                        {
                            return Err(TimelineAssetError::InvalidReference);
                        }
                        real_regular_file(&entry.path())?;
                        let metadata = fs::symlink_metadata(entry.path())?;
                        on_disk.insert(digest.to_owned(), metadata.len());
                    }
                }
            }
        }

        for reference in roots.values() {
            if !on_disk.contains_key(&reference.digest) {
                return Err(TimelineAssetError::Missing);
            }
            self.verify(reference)?;
        }
        let orphan_candidates = on_disk
            .into_iter()
            .filter(|(digest, _)| !roots.contains_key(digest))
            .map(|(digest, size)| OrphanAssetCandidate { digest, size })
            .collect();
        Ok(TimelineAssetInventory {
            retained: roots.into_values().collect(),
            orphan_candidates,
        })
    }

    /// Validate all uploaded parts before making the canonical append attempt.
    /// No receipt is issued when an asset is missing or corrupted. If the
    /// Timeline append fails, the unreferenced blob remains eligible for later
    /// reachability-based cleanup; no binary deletion is attempted here.
    pub fn append_checked(
        &self,
        timeline: &mut dyn TimelineRepository,
        branch: BranchId,
        expected_head: u64,
        operation_key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineAttachmentCommitError> {
        for draft in &drafts {
            if !matches!(
                draft.kind,
                EventKind::PlayerMessage
                    | EventKind::CharacterMessage
                    | EventKind::CharacterObservation
                    | EventKind::SessionNote
                    | EventKind::ReminderToolResult
                    | EventKind::MessageRevised
            ) {
                continue;
            }
            let content = canonical_message_content(draft);
            if let Some(content) = content {
                let parts = parse_message_content(content)
                    .map_err(|_| TimelineAssetError::InvalidReference)?;
                for part in parts {
                    if let Some(reference) = part.asset() {
                        self.verify(reference)?;
                    }
                }
            }
        }
        Ok(timeline.append(branch, expected_head, operation_key, command, drafts)?)
    }

    /// Check an explicitly referenced blob without returning private bytes.
    pub fn verify(&self, reference: &TimelineAssetRef) -> Result<(), TimelineAssetError> {
        self.stream_checked(reference, io::sink())
    }

    /// This is the only public content read: MitaView must already establish
    /// visibility, including hide, reset, revision and branch prefix semantics.
    pub fn read_visible(
        &self,
        view: &MitaView,
        event: EventId,
        reference: &TimelineAssetRef,
    ) -> Result<Vec<u8>, TimelineAssetError> {
        let visible = view.messages().into_iter().any(|message| {
            message.event_id == event
                && message
                    .fields
                    .get("content")
                    .and_then(|content| parse_message_content(content).ok())
                    .is_some_and(|parts| parts.iter().any(|part| part.asset() == Some(reference)))
        });
        if !visible {
            return Err(TimelineAssetError::NotVisible);
        }
        self.read_checked(reference)
    }

    /// Privileged transfer boundary. The caller must first prove the reference
    /// belongs to the explicitly selected canonical session being exported.
    pub(crate) fn read_for_archive(
        &self,
        reference: &TimelineAssetRef,
    ) -> Result<Vec<u8>, TimelineAssetError> {
        self.read_checked(reference)
    }

    fn path_for(&self, reference: &TimelineAssetRef) -> Result<PathBuf, TimelineAssetError> {
        reference.validate()?;
        if reference.size > self.max_bytes {
            return Err(TimelineAssetError::TooLarge);
        }
        Ok(self
            .directory
            .join("objects")
            .join(&reference.digest[..2])
            .join(&reference.digest))
    }

    fn check_object_directories(&self, path: &Path) -> Result<(), TimelineAssetError> {
        real_directory(&self.directory)?;
        real_directory(&self.directory.join("objects"))?;
        real_directory(path.parent().expect("asset has parent"))?;
        Ok(())
    }

    fn read_checked(&self, reference: &TimelineAssetRef) -> Result<Vec<u8>, TimelineAssetError> {
        let mut bytes = Vec::new();
        self.stream_checked(reference, &mut bytes)?;
        Ok(bytes)
    }

    fn stream_checked(
        &self,
        reference: &TimelineAssetRef,
        mut destination: impl Write,
    ) -> Result<(), TimelineAssetError> {
        let path = self.path_for(reference)?;
        self.check_object_directories(&path)?;
        real_regular_file(&path)?;
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(TimelineAssetError::Missing);
            }
            Err(error) => return Err(error.into()),
        };
        if file.metadata()?.len() != reference.size {
            return Err(TimelineAssetError::Corrupt);
        }
        let mut hasher = Context::new(&SHA256);
        hasher.update(reference.media_type.as_bytes());
        hasher.update(&[0]);
        let mut read_bytes = 0_u64;
        let mut buffer = [0_u8; CHUNK];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            read_bytes = read_bytes
                .checked_add(count as u64)
                .ok_or(TimelineAssetError::Corrupt)?;
            if read_bytes > reference.size {
                return Err(TimelineAssetError::Corrupt);
            }
            hasher.update(&buffer[..count]);
            destination.write_all(&buffer[..count])?;
        }
        if read_bytes != reference.size || digest_hex(hasher.finish()) != reference.digest {
            return Err(TimelineAssetError::Corrupt);
        }
        Ok(())
    }
}
