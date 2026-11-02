use std::fmt;

use super::{SettingsError, StoredSettings};

/// Schema version of the complete Rust settings envelope written by this build.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsTransferError {
    MalformedJson(String),
    NotFullSettingsTransfer(String),
    UnsupportedSchema { found: u64 },
    InvalidEnvelope(String),
    InvalidValue(String),
}

impl fmt::Display for SettingsTransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedJson(message) => {
                write!(formatter, "settings transfer is not valid JSON: {message}")
            }
            Self::NotFullSettingsTransfer(message) => write!(
                formatter,
                "settings transfer is not a complete schema {SETTINGS_SCHEMA_VERSION} envelope: {message}"
            ),
            Self::UnsupportedSchema { found } => write!(
                formatter,
                "unsupported settings schema {found}; this build supports schema {SETTINGS_SCHEMA_VERSION}"
            ),
            Self::InvalidEnvelope(message) => {
                write!(formatter, "invalid settings transfer: {message}")
            }
            Self::InvalidValue(message) => write!(formatter, "invalid settings values: {message}"),
        }
    }
}

impl std::error::Error for SettingsTransferError {}

impl From<SettingsTransferError> for SettingsError {
    fn from(error: SettingsTransferError) -> Self {
        match error {
            SettingsTransferError::InvalidValue(message) => Self::Validation(message),
            other => Self::Storage(other.to_string()),
        }
    }
}

/// A validated complete envelope holding every Rust AppSettings section,
/// the schema version and the revision. Settings from other implementations,
/// partial sections, presets and world backups are not this format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsTransfer {
    stored: StoredSettings,
}

impl SettingsTransfer {
    pub fn new(stored: StoredSettings) -> Result<Self, SettingsTransferError> {
        if stored.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsTransferError::UnsupportedSchema {
                found: u64::from(stored.schema_version),
            });
        }
        stored
            .settings
            .validate()
            .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))?;
        Ok(Self { stored })
    }

    /// Strictly decodes one complete envelope.
    ///
    /// Format errors, unrelated or partial documents, unsupported schemas,
    /// unknown/missing/duplicate fields and invalid values are distinct
    /// failures. Nothing is written by decoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, SettingsTransferError> {
        let document: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|error| SettingsTransferError::MalformedJson(error.to_string()))?;
        let object = document.as_object().ok_or_else(|| {
            SettingsTransferError::NotFullSettingsTransfer(
                "expected a JSON object with schema_version, revision and settings".into(),
            )
        })?;
        for field in ["schema_version", "revision", "settings"] {
            if !object.contains_key(field) {
                return Err(SettingsTransferError::NotFullSettingsTransfer(format!(
                    "missing {field}; partial documents cannot be imported"
                )));
            }
        }
        let found = object["schema_version"].as_u64().ok_or_else(|| {
            SettingsTransferError::InvalidEnvelope(
                "schema_version must be an unsigned integer".into(),
            )
        })?;
        if found != u64::from(SETTINGS_SCHEMA_VERSION) {
            return Err(SettingsTransferError::UnsupportedSchema { found });
        }
        let stored: StoredSettings = serde_json::from_slice(bytes)
            .map_err(|error| SettingsTransferError::InvalidEnvelope(error.to_string()))?;
        Self::new(stored)
    }

    /// Canonical bytes for this envelope. The same encoding is used for saves,
    /// exports and imports, so an exported file is a valid repository file.
    pub fn encode(&self) -> Result<Vec<u8>, SettingsTransferError> {
        self.stored
            .settings
            .validate()
            .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))?;
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
