//! A bounded, lossless transfer of retained MemoryRecord values only.
//! This format does not contain Timeline events, history, variables or graph edges.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterMemorySnapshot, MemoryEditResult},
    identity::CharacterId,
    memory::{
        CharacterMemories, MAX_MEMORY_EDIT_CHANGES, MemoryChange, MemoryCommand, MemoryId,
        MemoryRecord,
    },
};

pub const MEMORY_ARCHIVE_FORMAT: &str = "neuromita-retained-memory-records";
pub const MEMORY_ARCHIVE_VERSION: u32 = 2;
const LEGACY_MEMORY_ARCHIVE_VERSION: u32 = 1;
pub const MAX_ARCHIVE_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryArchive {
    format: String,
    version: u32,
    scope: String,
    source_character: CharacterId,
    records: Vec<MemoryRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checksum_sha256: Option<String>,
}

#[derive(Debug, Error)]
pub enum MemoryTransferError {
    #[error("archive exceeds the bounded record or byte limit")]
    LimitExceeded,
    #[error("invalid archive JSON: {0}")]
    Malformed(String),
    #[error("unsupported memory archive format, version, or scope")]
    UnsupportedFormat,
    #[error("archive contains duplicate memory IDs")]
    DuplicateId,
    #[error("memory archive payload checksum does not match its declared contents")]
    IntegrityMismatch,
    #[error("archive contains invalid or conflicting memories: {0}")]
    InvalidRecord(&'static str),
    #[error("memory import preview belongs to another character")]
    ForeignPreview,
    #[error("memory import preview changed since inspection")]
    StalePreview,
    #[error("explicit confirmation is required")]
    ConfirmationRequired,
    #[error("memory import cancelled before commit")]
    Cancelled,
    #[error("canonical character owner rejected the import: {0}")]
    Write(#[from] CharacterError),
    #[error("unable to serialize the memory archive: {0}")]
    Serialization(String),
}

/// Contains the original immutable version token and only the exact validated changes.
/// It must not be serialized or modified by a UI before the canonical apply.
#[derive(Debug, Clone)]
pub struct MemoryImportPreview {
    expected: CharacterMemorySnapshot,
    source: CharacterId,
    incoming: usize,
    commands: Vec<MemoryCommand>,
}

impl MemoryImportPreview {
    pub(crate) fn into_parts(self) -> (CharacterMemorySnapshot, Vec<MemoryCommand>) {
        (self.expected, self.commands)
    }

    pub fn source_character(&self) -> CharacterId {
        self.source
    }
    pub fn target_character(&self) -> CharacterId {
        self.expected.character()
    }
    pub fn incoming_records(&self) -> usize {
        self.incoming
    }
    pub fn new_records(&self) -> usize {
        self.commands.len()
    }
    pub fn identical_records(&self) -> usize {
        self.incoming - self.commands.len()
    }
    pub fn target_anchor(&self) -> crate::timeline::ContextAnchor {
        self.expected.anchor()
    }
}

fn verify(records: &[MemoryRecord]) -> Result<CharacterMemories, MemoryTransferError> {
    if records.len() > MAX_MEMORY_EDIT_CHANGES {
        return Err(MemoryTransferError::LimitExceeded);
    }
    let mut seen = BTreeSet::<MemoryId>::new();
    let mut memory = CharacterMemories::default();
    for record in records {
        if !seen.insert(record.id) {
            return Err(MemoryTransferError::DuplicateId);
        }
        memory
            .apply_committed(&MemoryChange::Upsert(record.clone()))
            .map_err(MemoryTransferError::InvalidRecord)?;
        memory
            .ensure_unique()
            .map_err(MemoryTransferError::InvalidRecord)?;
    }
    Ok(memory)
}

fn archive_checksum(archive: &MemoryArchive) -> Result<String, MemoryTransferError> {
    let payload = serde_json::to_vec(&(
        &archive.format,
        archive.version,
        &archive.scope,
        archive.source_character,
        &archive.records,
    ))
    .map_err(|error| MemoryTransferError::Serialization(error.to_string()))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &payload);
    Ok(digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn export_memory(snapshot: &CharacterMemorySnapshot) -> Result<Vec<u8>, MemoryTransferError> {
    let _ = verify(snapshot.records())?;
    let archive = MemoryArchive {
        format: MEMORY_ARCHIVE_FORMAT.into(),
        version: MEMORY_ARCHIVE_VERSION,
        scope: "retained_memory_records".into(),
        source_character: snapshot.character(),
        records: snapshot.records().to_vec(),
        checksum_sha256: None,
    };
    let mut archive = archive;
    archive.checksum_sha256 = Some(archive_checksum(&archive)?);
    let mut bytes = serde_json::to_vec_pretty(&archive)
        .map_err(|e| MemoryTransferError::Serialization(e.to_string()))?;
    if bytes.len().saturating_add(1) > MAX_ARCHIVE_BYTES {
        return Err(MemoryTransferError::LimitExceeded);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn preview_memory_import(
    expected: CharacterMemorySnapshot,
    bytes: &[u8],
) -> Result<MemoryImportPreview, MemoryTransferError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(MemoryTransferError::LimitExceeded);
    }
    let archive: MemoryArchive =
        serde_json::from_slice(bytes).map_err(|e| MemoryTransferError::Malformed(e.to_string()))?;
    if archive.format != MEMORY_ARCHIVE_FORMAT
        || !matches!(
            archive.version,
            LEGACY_MEMORY_ARCHIVE_VERSION | MEMORY_ARCHIVE_VERSION
        )
        || archive.scope != "retained_memory_records"
    {
        return Err(MemoryTransferError::UnsupportedFormat);
    }
    let _ = verify(&archive.records)?;
    match archive.version {
        LEGACY_MEMORY_ARCHIVE_VERSION if archive.checksum_sha256.is_none() => {}
        MEMORY_ARCHIVE_VERSION
            if archive.checksum_sha256.as_deref() == Some(archive_checksum(&archive)?.as_str()) => {
        }
        _ => return Err(MemoryTransferError::IntegrityMismatch),
    }
    let mut target = CharacterMemories::default();
    for record in expected.records() {
        target
            .apply_committed(&MemoryChange::Upsert(record.clone()))
            .map_err(MemoryTransferError::InvalidRecord)?;
        target
            .ensure_unique()
            .map_err(MemoryTransferError::InvalidRecord)?;
    }
    let commands: Vec<_> = archive
        .records
        .iter()
        .cloned()
        .map(|record| MemoryCommand::ImportRecord { record })
        .collect();
    let changes = target
        .resolve(&commands)
        .map_err(MemoryTransferError::InvalidRecord)?;
    let commands = changes
        .into_iter()
        .map(|change| match change {
            MemoryChange::Upsert(record) => MemoryCommand::ImportRecord { record },
            _ => unreachable!("exact import only yields upserts"),
        })
        .collect();
    Ok(MemoryImportPreview {
        expected,
        source: archive.source_character,
        incoming: archive.records.len(),
        commands,
    })
}

/// Confirmation and cancellation are checked before owner dispatch; once sent,
/// the canonical character/Timestamp selection guard owns the atomic outcome.
pub async fn apply_memory_import(
    owner: &CharacterHandle,
    plan: MemoryImportPreview,
    confirmed: bool,
    cancel: &CancellationToken,
) -> Result<MemoryEditResult, MemoryTransferError> {
    if !confirmed {
        return Err(MemoryTransferError::ConfirmationRequired);
    }
    if cancel.is_cancelled() {
        return Err(MemoryTransferError::Cancelled);
    }
    let current = owner.memories().await?;
    if current.character() != plan.target_character() {
        return Err(MemoryTransferError::ForeignPreview);
    }
    if !plan.expected.matches_version(&current) || plan.expected.anchor() != current.anchor() {
        return Err(MemoryTransferError::StalePreview);
    }
    if cancel.is_cancelled() {
        return Err(MemoryTransferError::Cancelled);
    }
    if plan.commands.is_empty() {
        return Ok(MemoryEditResult {
            applied: Vec::new(),
            snapshot: current,
        });
    }
    owner
        .edit_memories(plan.expected, plan.commands)
        .await
        .map_err(Into::into)
}
