use serde::Deserialize;

use crate::migration::Migration;

use super::{
    AppSettings, CaptureSettings, CharacterSettings, CharactersSettings, InterfaceSettings,
    OperationPresets, PresetId, PresetSettings, SETTINGS_SCHEMA_VERSION, SettingsImportFormat,
    SettingsTransferError, StoredSettings, VoiceSettings,
};

/// The same explicit schema upgrades serve startup and prepared imports.
pub struct RustSettingsMigration;

impl Migration<&[u8]> for RustSettingsMigration {
    type Output = StoredSettings;
    type Error = SettingsTransferError;

    fn migrate(&self, bytes: &[u8]) -> Result<StoredSettings, SettingsTransferError> {
        let document: serde_json::Value = serde_json::from_slice(bytes)
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
        let header: SchemaHeader = decode(bytes)?;
        let stored = match header.schema_version {
            1 => decode::<StoredSettingsV1>(bytes)?.upgrade(),
            SETTINGS_SCHEMA_VERSION => decode(bytes)?,
            found => return Err(SettingsTransferError::UnsupportedSchema { found }),
        };
        stored
            .settings
            .validate()
            .map_err(|error| SettingsTransferError::InvalidValue(error.to_string()))?;
        Ok(stored)
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
    fn upgrade(self) -> StoredSettings {
        debug_assert_eq!(self.schema_version, 1);
        StoredSettings {
            schema_version: SETTINGS_SCHEMA_VERSION,
            revision: self.revision,
            settings: AppSettings {
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
