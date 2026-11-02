use thiserror::Error;

use crate::migration::{Migration, MigrationError};

use super::{
    PythonSettingsMigration, RustSettingsMigration, SETTINGS_SCHEMA_VERSION, SettingsError,
    StoredSettings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsImportFormat {
    Rust,
    Python,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SettingsTransferError {
    #[error("settings transfer is not valid JSON: {0}")]
    MalformedJson(String),
    #[error("settings transfer is not a complete envelope: {0}")]
    NotFullSettingsTransfer(String),
    #[error("unsupported settings schema {found}; current schema is {SETTINGS_SCHEMA_VERSION}")]
    UnsupportedSchema { found: u32 },
    #[error("invalid settings transfer: {0}")]
    InvalidEnvelope(String),
    #[error("invalid settings values: {0}")]
    InvalidValue(String),
    #[error("{format:?} settings migration is not implemented")]
    NotImplemented { format: SettingsImportFormat },
    #[error(transparent)]
    Migration(MigrationError),
}

impl From<MigrationError> for SettingsTransferError {
    fn from(error: MigrationError) -> Self {
        match error {
            MigrationError::UnsupportedVersion { found, .. } => Self::UnsupportedSchema { found },
            other => Self::Migration(other),
        }
    }
}

impl From<SettingsTransferError> for SettingsError {
    fn from(error: SettingsTransferError) -> Self {
        match error {
            SettingsTransferError::InvalidValue(message) => Self::Validation(message),
            other => Self::Storage(other.to_string()),
        }
    }
}

/// A complete validated current-schema envelope, ready to export or apply.
/// Preparation never mutates the live owner or its repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsTransfer {
    stored: StoredSettings,
}

impl SettingsTransfer {
    pub fn new(stored: StoredSettings) -> Result<Self, SettingsTransferError> {
        if stored.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsTransferError::UnsupportedSchema {
                found: stored.schema_version,
            });
        }
        stored
            .settings
            .validate()
            .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))?;
        Ok(Self { stored })
    }

    pub fn prepare(
        format: SettingsImportFormat,
        bytes: &[u8],
    ) -> Result<Self, SettingsTransferError> {
        let stored = match format {
            SettingsImportFormat::Rust => RustSettingsMigration.migrate(bytes)?,
            SettingsImportFormat::Python => PythonSettingsMigration.migrate(bytes)?,
        };
        Self::new(stored)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, SettingsTransferError> {
        Self::prepare(SettingsImportFormat::Rust, bytes)
    }

    pub fn encode(&self) -> Result<Vec<u8>, SettingsTransferError> {
        let mut bytes = serde_json::to_vec_pretty(&self.stored)
            .map_err(|error| SettingsTransferError::InvalidEnvelope(error.to_string()))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn stored(&self) -> &StoredSettings {
        &self.stored
    }

    pub fn into_stored(self) -> StoredSettings {
        self.stored
    }
}
