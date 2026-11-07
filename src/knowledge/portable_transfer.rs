//! An atomic, scoped export/import of retained memories plus user-authored variables.
//! Not a Timeline/SessionArchive export; history is deliberately excluded.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    memory_transfer::{self, MAX_ARCHIVE_BYTES, MemoryImportPreview, MemoryTransferError},
    variable_transfer::{
        self, MAX_VARIABLE_ARCHIVE_BYTES, VariableImportPreview, VariableTransferError,
    },
};
use crate::{
    character::{CharacterHandle, CharacterPortableResult, CharacterPortableSnapshot},
    identity::CharacterId,
};

pub const PORTABLE_ARCHIVE_FORMAT: &str = "neuromita-character-knowledge";
pub const PORTABLE_ARCHIVE_VERSION: u32 = 1;
pub const MAX_PORTABLE_ARCHIVE_BYTES: usize =
    MAX_ARCHIVE_BYTES + MAX_VARIABLE_ARCHIVE_BYTES + 65_536;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableArchive {
    format: String,
    version: u32,
    scope: String,
    source_character: CharacterId,
    memory_archive: String,
    variables_archive: String,
    checksum_sha256: String,
}

#[derive(Debug, Error)]
pub enum PortableTransferError {
    #[error("composite archive exceeds its size limit")]
    LimitExceeded,
    #[error("malformed composite JSON: {0}")]
    Malformed(String),
    #[error("unsupported composite archive version/scope")]
    UnsupportedFormat,
    #[error("composite payload integrity mismatch")]
    IntegrityMismatch,
    #[error("embedded archives have inconsistent source character")]
    InconsistentSources,
    #[error("composite import preview belongs to another character")]
    ForeignPreview,
    #[error("composite import preview is stale")]
    StalePreview,
    #[error("explicit import confirmation required")]
    ConfirmationRequired,
    #[error("composite import cancelled before dispatch")]
    Cancelled,
    #[error(transparent)]
    Memory(#[from] MemoryTransferError),
    #[error(transparent)]
    Variables(#[from] VariableTransferError),
    #[error(transparent)]
    Character(#[from] crate::character::CharacterError),
    #[error("composite archive serialization failed: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone)]
pub struct PortableImportPreview {
    source: CharacterId,
    target: CharacterId,
    memory: MemoryImportPreview,
    variables: VariableImportPreview,
}

impl PortableImportPreview {
    pub fn source_character(&self) -> CharacterId {
        self.source
    }
    pub fn target_character(&self) -> CharacterId {
        self.target
    }
    pub fn incoming_memories(&self) -> usize {
        self.memory.incoming_records()
    }
    pub fn new_memories(&self) -> usize {
        self.memory.new_records()
    }
    pub fn incoming_variables(&self) -> usize {
        self.variables.incoming_count()
    }
    pub fn new_variables(&self) -> usize {
        self.variables.added_count()
    }
    pub fn target_anchor(&self) -> crate::timeline::ContextAnchor {
        self.memory.target_anchor()
    }
}

fn checksum(archive: &PortableArchive) -> Result<String, PortableTransferError> {
    let payload = serde_json::to_vec(&(
        &archive.format,
        archive.version,
        &archive.scope,
        archive.source_character,
        &archive.memory_archive,
        &archive.variables_archive,
    ))
    .map_err(|error| PortableTransferError::Serialization(error.to_string()))?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &payload)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn export_portable(
    snapshot: &CharacterPortableSnapshot,
) -> Result<Vec<u8>, PortableTransferError> {
    if snapshot.memory.character() != snapshot.variables.character()
        || snapshot.memory.anchor() != snapshot.variables.anchor()
    {
        return Err(PortableTransferError::StalePreview);
    }
    let memory = memory_transfer::export_memory(&snapshot.memory)?;
    let variables = variable_transfer::export_variables(&snapshot.variables)?;
    let mut archive = PortableArchive {
        format: PORTABLE_ARCHIVE_FORMAT.to_owned(),
        version: PORTABLE_ARCHIVE_VERSION,
        scope: "retained_memory_and_user_variables".into(),
        source_character: snapshot.memory.character(),
        memory_archive: String::from_utf8(memory)
            .map_err(|error| PortableTransferError::Serialization(error.to_string()))?,
        variables_archive: String::from_utf8(variables)
            .map_err(|error| PortableTransferError::Serialization(error.to_string()))?,
        checksum_sha256: String::new(),
    };
    archive.checksum_sha256 = checksum(&archive)?;
    let mut bytes = serde_json::to_vec_pretty(&archive)
        .map_err(|error| PortableTransferError::Serialization(error.to_string()))?;
    if bytes.len() + 1 > MAX_PORTABLE_ARCHIVE_BYTES {
        return Err(PortableTransferError::LimitExceeded);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn preview_portable_import(
    expected: CharacterPortableSnapshot,
    bytes: &[u8],
) -> Result<PortableImportPreview, PortableTransferError> {
    if bytes.len() > MAX_PORTABLE_ARCHIVE_BYTES {
        return Err(PortableTransferError::LimitExceeded);
    }
    let archive: PortableArchive = serde_json::from_slice(bytes)
        .map_err(|error| PortableTransferError::Malformed(error.to_string()))?;
    if archive.format != PORTABLE_ARCHIVE_FORMAT
        || archive.version != PORTABLE_ARCHIVE_VERSION
        || archive.scope != "retained_memory_and_user_variables"
    {
        return Err(PortableTransferError::UnsupportedFormat);
    }
    if checksum(&archive)? != archive.checksum_sha256 {
        return Err(PortableTransferError::IntegrityMismatch);
    }
    if expected.memory.character() != expected.variables.character()
        || expected.memory.anchor() != expected.variables.anchor()
    {
        return Err(PortableTransferError::StalePreview);
    }
    let memory =
        memory_transfer::preview_memory_import(expected.memory, archive.memory_archive.as_bytes())?;
    let variables = variable_transfer::preview_variable_import(
        expected.variables,
        archive.variables_archive.as_bytes(),
    )?;
    if archive.source_character != memory.source_character()
        || archive.source_character != variables.source_character()
    {
        return Err(PortableTransferError::InconsistentSources);
    }
    if memory.target_character() != variables.target_character()
        || memory.target_anchor() != variables.target_anchor()
    {
        return Err(PortableTransferError::StalePreview);
    }
    Ok(PortableImportPreview {
        source: archive.source_character,
        target: memory.target_character(),
        memory,
        variables,
    })
}

pub async fn apply_portable_import(
    owner: &CharacterHandle,
    preview: PortableImportPreview,
    confirmed: bool,
    cancel: &CancellationToken,
) -> Result<CharacterPortableResult, PortableTransferError> {
    if !confirmed {
        return Err(PortableTransferError::ConfirmationRequired);
    }
    if cancel.is_cancelled() {
        return Err(PortableTransferError::Cancelled);
    }
    let current = owner.portable_snapshot().await?;
    if current.memory.character() != preview.target {
        return Err(PortableTransferError::ForeignPreview);
    }
    if current.memory.anchor() != preview.target_anchor() {
        return Err(PortableTransferError::StalePreview);
    }
    let (memory, commands) = preview.memory.into_parts();
    let (variables, additions) = preview.variables.into_parts();
    if cancel.is_cancelled() {
        return Err(PortableTransferError::Cancelled);
    }
    owner
        .import_portable(
            CharacterPortableSnapshot { memory, variables },
            commands,
            additions,
        )
        .await
        .map_err(|error| match error {
            crate::character::CharacterError::StaleState => PortableTransferError::StalePreview,
            other => PortableTransferError::Character(other),
        })
}
