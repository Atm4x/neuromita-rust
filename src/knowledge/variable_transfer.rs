//! A character-scoped transfer of user-authored canonical variables only.
//! Computed personality metrics and session history are explicitly excluded.

use std::collections::BTreeMap;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterVariablesSnapshot},
    identity::CharacterId,
};

pub const VARIABLE_ARCHIVE_FORMAT: &str = "neuromita-user-character-variables";
pub const VARIABLE_ARCHIVE_VERSION: u32 = 1;
pub const MAX_VARIABLE_ARCHIVE_BYTES: usize = 262_144;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariableArchive {
    format: String,
    version: u32,
    scope: String,
    source_character: CharacterId,
    #[serde(deserialize_with = "deserialize_unique_variables")]
    variables: BTreeMap<String, Value>,
    checksum_sha256: String,
}

fn deserialize_unique_variables<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, Value>, D::Error>
where
    D: Deserializer<'de>,
{
    struct UniqueVariables;

    impl<'de> Visitor<'de> for UniqueVariables {
        type Value = BTreeMap<String, Value>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a JSON object containing unique variable names")
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate variable name"));
                }
                if values.len() > 64 {
                    return Err(serde::de::Error::custom("too many variable names"));
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueVariables)
}

#[derive(Debug, Error)]
pub enum VariableTransferError {
    #[error("variable archive exceeds supported size or count")]
    LimitExceeded,
    #[error("malformed variable archive: {0}")]
    Malformed(String),
    #[error("unsupported variable archive format or scope")]
    UnsupportedFormat,
    #[error("variable archive content checksum differs from its payload")]
    IntegrityMismatch,
    #[error("variable archive contains invalid variable names or values: {0}")]
    InvalidVariable(&'static str),
    #[error("conflicting variable already exists in target character")]
    Conflict,
    #[error("import belongs to another target character")]
    ForeignPreview,
    #[error("variable import preview is stale")]
    StalePreview,
    #[error("explicit confirmation is required")]
    ConfirmationRequired,
    #[error("variable import was cancelled before dispatch")]
    Cancelled,
    #[error(transparent)]
    Character(#[from] CharacterError),
    #[error("archive serialization failed: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone)]
pub struct VariableImportPreview {
    expected: CharacterVariablesSnapshot,
    source: CharacterId,
    incoming_count: usize,
    added: BTreeMap<String, Value>,
}

impl VariableImportPreview {
    pub fn target_character(&self) -> CharacterId {
        self.expected.character()
    }
    pub fn source_character(&self) -> CharacterId {
        self.source
    }
    pub fn incoming_count(&self) -> usize {
        self.incoming_count
    }
    pub fn added_count(&self) -> usize {
        self.added.len()
    }
    pub fn identical_count(&self) -> usize {
        self.incoming_count - self.added.len()
    }
    pub fn target_anchor(&self) -> crate::timeline::ContextAnchor {
        self.expected.anchor()
    }
}

fn validate_variables(values: &BTreeMap<String, Value>) -> Result<(), VariableTransferError> {
    if !values.is_empty() {
        crate::character::projection::validate_character_variables(values)
            .map_err(VariableTransferError::InvalidVariable)?;
    }
    Ok(())
}

fn checksum(archive: &VariableArchive) -> Result<String, VariableTransferError> {
    let bytes = serde_json::to_vec(&(
        &archive.format,
        archive.version,
        &archive.scope,
        archive.source_character,
        &archive.variables,
    ))
    .map_err(|error| VariableTransferError::Serialization(error.to_string()))?;
    Ok(ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn export_variables(
    snapshot: &CharacterVariablesSnapshot,
) -> Result<Vec<u8>, VariableTransferError> {
    validate_variables(snapshot.values())?;
    let mut archive = VariableArchive {
        format: VARIABLE_ARCHIVE_FORMAT.into(),
        version: VARIABLE_ARCHIVE_VERSION,
        scope: "user_variables_only".into(),
        source_character: snapshot.character(),
        variables: snapshot.values().clone(),
        checksum_sha256: String::new(),
    };
    archive.checksum_sha256 = checksum(&archive)?;
    let mut result = serde_json::to_vec_pretty(&archive)
        .map_err(|error| VariableTransferError::Serialization(error.to_string()))?;
    if result.len() + 1 > MAX_VARIABLE_ARCHIVE_BYTES {
        return Err(VariableTransferError::LimitExceeded);
    }
    result.push(b'\n');
    Ok(result)
}

pub fn preview_variable_import(
    expected: CharacterVariablesSnapshot,
    bytes: &[u8],
) -> Result<VariableImportPreview, VariableTransferError> {
    if bytes.len() > MAX_VARIABLE_ARCHIVE_BYTES {
        return Err(VariableTransferError::LimitExceeded);
    }
    let archive: VariableArchive = serde_json::from_slice(bytes)
        .map_err(|error| VariableTransferError::Malformed(error.to_string()))?;
    if archive.format != VARIABLE_ARCHIVE_FORMAT
        || archive.version != VARIABLE_ARCHIVE_VERSION
        || archive.scope != "user_variables_only"
    {
        return Err(VariableTransferError::UnsupportedFormat);
    }
    validate_variables(&archive.variables)?;
    if archive.checksum_sha256 != checksum(&archive)? {
        return Err(VariableTransferError::IntegrityMismatch);
    }
    let mut merged = expected.values().clone();
    let mut added = BTreeMap::new();
    for (key, value) in &archive.variables {
        match merged.get(key) {
            Some(existing) if existing == value => {}
            Some(_) => return Err(VariableTransferError::Conflict),
            None => {
                merged.insert(key.clone(), value.clone());
                added.insert(key.clone(), value.clone());
            }
        }
    }
    validate_variables(&merged)?;
    Ok(VariableImportPreview {
        expected,
        source: archive.source_character,
        incoming_count: archive.variables.len(),
        added,
    })
}

pub async fn apply_variable_import(
    owner: &CharacterHandle,
    plan: VariableImportPreview,
    confirmed: bool,
    cancel: &CancellationToken,
) -> Result<CharacterVariablesSnapshot, VariableTransferError> {
    if !confirmed {
        return Err(VariableTransferError::ConfirmationRequired);
    }
    if cancel.is_cancelled() {
        return Err(VariableTransferError::Cancelled);
    }
    let current = owner.user_variables().await?;
    if current.character() != plan.target_character() {
        return Err(VariableTransferError::ForeignPreview);
    }
    if current.anchor() != plan.expected.anchor() || current.values() != plan.expected.values() {
        return Err(VariableTransferError::StalePreview);
    }
    if cancel.is_cancelled() {
        return Err(VariableTransferError::Cancelled);
    }
    owner
        .import_variables(plan.expected, plan.added)
        .await
        .map_err(|error| match error {
            CharacterError::StaleState => VariableTransferError::StalePreview,
            CharacterError::InvalidVariables(reason) => {
                VariableTransferError::InvalidVariable(reason)
            }
            other => VariableTransferError::Character(other),
        })
}
