use serde::{Deserialize, Serialize};

use crate::migration::{
    Migration, MigrationError, MigrationSchema, MigrationService, MigrationStep, StartupMigration,
    VersionedData,
};
use std::sync::Arc;

use super::{
    AppSettings, CaptureSettings, CharacterSettings, CharactersSettings, InterfaceSettings,
    OperationPresets, PresetId, PresetSettings, RemindersSettings, SETTINGS_SCHEMA_VERSION,
    SettingsImportFormat, SettingsRepository, SettingsTransferError, StoredSettings, VoiceSettings,
};

/// The same explicit schema upgrades serve startup and prepared imports.
pub struct RustSettingsMigration;

impl RustSettingsMigration {
    pub fn service() -> MigrationService<Vec<u8>> {
        MigrationService::new(
            SettingsSchema,
            vec![Box::new(SettingsV1ToV2), Box::new(SettingsV2ToV3)],
        )
        .expect("the built-in settings migration plan is valid")
    }

    pub(crate) fn document(
        bytes: Vec<u8>,
    ) -> Result<VersionedData<Vec<u8>>, SettingsTransferError> {
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| SettingsTransferError::MalformedJson(error.to_string()))?;
        let object = document.as_object().ok_or_else(|| {
            SettingsTransferError::NotFullSettingsTransfer("expected an envelope object".into())
        })?;
        for field in ["schema_version", "revision", "settings"] {
            if !object.contains_key(field) {
                return Err(SettingsTransferError::NotFullSettingsTransfer(format!(
                    "missing {field}"
                )));
            }
        }
        let header: SchemaHeader = decode(&bytes)?;
        Ok(VersionedData {
            version: header.schema_version,
            data: bytes,
        })
    }
}

impl Migration<&[u8]> for RustSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;

    fn migrate(&self, bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        let current = Self::service().prepare(Self::document(bytes.to_vec())?)?;
        decode(&current.data)
    }
}

pub(crate) struct SettingsSchema;

impl MigrationSchema<Vec<u8>> for SettingsSchema {
    fn current_version(&self) -> u32 {
        SETTINGS_SCHEMA_VERSION
    }

    fn validate(&self, value: &VersionedData<Vec<u8>>) -> Result<(), MigrationError> {
        let validate = || -> Result<(), SettingsTransferError> {
            let header: SchemaHeader = decode(&value.data)?;
            if header.schema_version != value.version {
                return Err(SettingsTransferError::InvalidEnvelope(
                    "envelope and migration versions differ".into(),
                ));
            }
            let stored: StoredSettings = match value.version {
                1 => decode::<StoredSettingsV1>(&value.data)?
                    .upgrade_v2()
                    .into_current(),
                2 => decode::<StoredSettingsV2>(&value.data)?.into_current(),
                SETTINGS_SCHEMA_VERSION => decode(&value.data)?,
                found => return Err(SettingsTransferError::UnsupportedSchema { found }),
            };
            stored
                .settings
                .validate()
                .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))
        };
        validate().map_err(|error| MigrationError::InvalidData(error.to_string()))
    }
}

struct SettingsV1ToV2;

impl MigrationStep<Vec<u8>> for SettingsV1ToV2 {
    fn source_version(&self) -> u32 {
        1
    }
    fn target_version(&self) -> u32 {
        2
    }

    fn migrate(&self, bytes: Vec<u8>) -> Result<Vec<u8>, MigrationError> {
        let upgraded = decode::<StoredSettingsV1>(&bytes)
            .map_err(|error| MigrationError::InvalidData(error.to_string()))?
            .upgrade_v2();
        serde_json::to_vec(&upgraded)
            .map_err(|error| MigrationError::InvalidData(error.to_string()))
    }
}

struct SettingsV2ToV3;

impl MigrationStep<Vec<u8>> for SettingsV2ToV3 {
    fn source_version(&self) -> u32 {
        2
    }
    fn target_version(&self) -> u32 {
        3
    }

    fn migrate(&self, bytes: Vec<u8>) -> Result<Vec<u8>, MigrationError> {
        let upgraded = decode::<StoredSettingsV2>(&bytes)
            .map_err(|error| MigrationError::InvalidData(error.to_string()))?
            .into_current();
        serde_json::to_vec(&upgraded)
            .map_err(|error| MigrationError::InvalidData(error.to_string()))
    }
}

pub(crate) struct SettingsStartupMigration(pub Arc<dyn SettingsRepository>);

impl StartupMigration for SettingsStartupMigration {
    fn name(&self) -> &str {
        "settings"
    }

    fn run(&self) -> Result<(), MigrationError> {
        self.0
            .migrate()
            .map_err(|error| MigrationError::Storage(error.to_string()))
    }
}

/// Extension point only; no Python values are interpreted or replaced yet.
pub struct PythonSettingsMigration;

impl Migration<&[u8]> for PythonSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;

    fn migrate(&self, _bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        Err(SettingsTransferError::NotImplemented {
            format: SettingsImportFormat::Python,
        })
    }
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, SettingsTransferError> {
    serde_json::from_slice(bytes)
        .map_err(|error| SettingsTransferError::InvalidEnvelope(error.to_string()))
}

#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettingsV1 {
    schema_version: u32,
    revision: u64,
    settings: AppSettingsV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppSettingsV1 {
    #[serde(default)]
    presets: PresetSettingsV1,
    voice: VoiceSettings,
    capture: CaptureSettings,
    interface: InterfaceSettings,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetSettingsV1 {
    active: PresetId,
    #[serde(default)]
    characters: CharacterPresetAssignmentsV1,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CharacterPresetAssignmentsV1 {
    mita: OperationPresets,
    kind_mita: OperationPresets,
}

impl StoredSettingsV1 {
    fn upgrade_v2(self) -> StoredSettingsV2 {
        debug_assert_eq!(self.schema_version, 1);
        StoredSettingsV2 {
            schema_version: 2,
            revision: self.revision,
            settings: AppSettingsV2 {
                presets: PresetSettings {
                    active: self.settings.presets.active,
                },
                characters: CharactersSettings {
                    mita: CharacterSettings {
                        presets: self.settings.presets.characters.mita,
                    },
                    kind_mita: CharacterSettings {
                        presets: self.settings.presets.characters.kind_mita,
                    },
                },
                voice: self.settings.voice,
                capture: self.settings.capture,
                interface: self.settings.interface,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettingsV2 {
    schema_version: u32,
    revision: u64,
    settings: AppSettingsV2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppSettingsV2 {
    presets: PresetSettings,
    characters: CharactersSettings,
    voice: VoiceSettings,
    capture: CaptureSettings,
    interface: InterfaceSettings,
}

impl StoredSettingsV2 {
    fn into_current(self) -> StoredSettings {
        debug_assert_eq!(self.schema_version, 2);
        StoredSettings {
            schema_version: 3,
            revision: self.revision,
            settings: AppSettings {
                presets: self.settings.presets,
                characters: self.settings.characters,
                voice: self.settings.voice,
                capture: self.settings.capture,
                interface: self.settings.interface,
                reminders: RemindersSettings::initial_profile(),
            },
        }
    }
}
