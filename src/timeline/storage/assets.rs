//! Content-addressed binary attachments for canonical Timeline messages.
//! The directory is injected by the application composition root; this module
//! never chooses an installation path or owns message/branch visibility.

use std::{
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
        parse_message_content,
    },
    view::MitaView,
};

const CHUNK: usize = 64 * 1024;

pub use super::super::domain::TimelineAssetRef;

#[derive(Debug, Error)]
pub enum TimelineAssetError {
    #[error("invalid Timeline asset reference or media type")]
    InvalidReference,
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
        fs::create_dir_all(&directory)?;
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
        let staging = self.directory.join("staging");
        fs::create_dir_all(&staging)?;
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
        fs::create_dir_all(destination.parent().expect("asset has parent"))?;
        match fs::hard_link(&staged.path, &destination) {
            Ok(()) => {
                // The file is durable before the link becomes visible. A crash
                // before the Timeline append only produces an unreferenced blob.
                #[cfg(unix)]
                File::open(destination.parent().expect("asset has parent"))?.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify(&reference)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(reference)
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
            ) {
                continue;
            }
            let content = draft.payload.get("content").or_else(|| {
                draft
                    .payload
                    .get("message")
                    .and_then(|message| message.get("content"))
            });
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
        self.read_checked(reference).map(|_| ())
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

    fn read_checked(&self, reference: &TimelineAssetRef) -> Result<Vec<u8>, TimelineAssetError> {
        let path = self.path_for(reference)?;
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
        let capacity = usize::try_from(reference.size).map_err(|_| TimelineAssetError::TooLarge)?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut buffer = [0_u8; CHUNK];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > capacity {
                return Err(TimelineAssetError::Corrupt);
            }
            hasher.update(&buffer[..count]);
            bytes.extend_from_slice(&buffer[..count]);
        }
        if bytes.len() as u64 != reference.size || digest_hex(hasher.finish()) != reference.digest {
            return Err(TimelineAssetError::Corrupt);
        }
        Ok(bytes)
    }
}
