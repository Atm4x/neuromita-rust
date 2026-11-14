//! Read-only module bootstrap and hardware contracts, independent of installed inference runtimes.

use std::collections::HashSet;

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{AiService, layer::valid_segment};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModuleId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HardwareId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExecutionVariantId(String);

macro_rules! validated_id {
    ($id:ident) => {
        impl TryFrom<String> for $id {
            type Error = ModuleContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                if !valid_segment(&value) {
                    return Err(ModuleContractError::InvalidIdentity(value));
                }
                Ok(Self(value))
            }
        }

        impl From<$id> for String {
            fn from(value: $id) -> Self {
                value.0
            }
        }

        impl $id {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

validated_id!(ModuleId);
validated_id!(ModelId);
validated_id!(HardwareId);
validated_id!(ExecutionVariantId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModuleBootstrap {
    PythonSdk {
        python_minor: String,
        sdk_api_version: u32,
    },
    Embedded,
    NativeExecutable {
        relative_path: String,
        sha256: String,
    },
}

impl ModuleBootstrap {
    pub fn python_sdk_312() -> Self {
        Self::PythonSdk {
            python_minor: "3.12".into(),
            sdk_api_version: super::sdk::SDK_API_VERSION,
        }
    }

    fn validate(&self) -> Result<(), ModuleContractError> {
        match self {
            Self::PythonSdk {
                python_minor,
                sdk_api_version,
            } if python_minor == "3.12" && *sdk_api_version == super::sdk::SDK_API_VERSION => {
                Ok(())
            }
            Self::Embedded => Ok(()),
            Self::NativeExecutable {
                relative_path,
                sha256,
            } if safe_relative_path(relative_path)
                && sha256.len() == 64
                && sha256.bytes().all(|c| c.is_ascii_hexdigit()) =>
            {
                Ok(())
            }
            _ => Err(ModuleContractError::InvalidBootstrap),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleModel {
    pub id: ModelId,
    pub revision: String,
    pub label: String,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelModuleDeclaration {
    pub id: ModuleId,
    pub name: String,
    pub service: AiService,
    pub manifest_sha256: String,
    pub bootstrap: ModuleBootstrap,
    pub models: Vec<ModuleModel>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModuleContractError {
    #[error("invalid typed identity: {0}")]
    InvalidIdentity(String),
    #[error("invalid or unsupported module manifest")]
    InvalidManifest,
    #[error("invalid module bootstrap requirements")]
    InvalidBootstrap,
    #[error("invalid or repeated model declaration")]
    InvalidModel,
    #[error("unknown hardware identity")]
    UnknownHardware,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeclaredModel {
    id: String,
    revision: String,
    label: String,
    #[serde(default)]
    languages: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PythonPluginManifest {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub api_version: u32,
    pub entrypoint: String,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub permissions: Option<Vec<String>>,
    #[serde(default)]
    pub models: Vec<DeclaredModel>,
    pub bootstrap: Option<ModuleBootstrap>,
}

impl ModelModuleDeclaration {
    pub(crate) fn from_python_manifest(
        manifest: &PythonPluginManifest,
        original: &[u8],
    ) -> Result<Self, ModuleContractError> {
        if manifest.api_version != 1
            || manifest.entrypoint != "plugin.py"
            || original.len() > 64 * 1024
            || manifest.name.trim().is_empty()
            || manifest.name.len() > 128
        {
            return Err(ModuleContractError::InvalidManifest);
        }
        let service = match manifest.kind.as_str() {
            "tts" => AiService::Tts,
            "asr" => AiService::Asr,
            "embedding" => AiService::Embedding,
            "reranker" => AiService::Reranker,
            "beats" => AiService::Beats,
            _ => return Err(ModuleContractError::InvalidManifest),
        };
        let bootstrap = manifest
            .bootstrap
            .clone()
            .unwrap_or_else(ModuleBootstrap::python_sdk_312);
        if bootstrap != ModuleBootstrap::python_sdk_312() {
            return Err(ModuleContractError::InvalidBootstrap);
        }
        bootstrap.validate()?;
        if manifest.models.len() > 128 {
            return Err(ModuleContractError::InvalidModel);
        }
        let mut models = Vec::with_capacity(manifest.models.len());
        let mut seen = HashSet::new();
        for model in &manifest.models {
            let id = ModelId::try_from(model.id.clone())?;
            if !seen.insert(id.clone())
                || !valid_revision(&model.revision)
                || model.label.trim().is_empty()
                || model.label.len() > 256
                || model.languages.len() > 32
                || model
                    .languages
                    .iter()
                    .any(|language| !valid_language(language))
                || model.languages.iter().collect::<HashSet<_>>().len() != model.languages.len()
            {
                return Err(ModuleContractError::InvalidModel);
            }
            models.push(ModuleModel {
                id,
                revision: model.revision.clone(),
                label: model.label.clone(),
                languages: model.languages.clone(),
            });
        }
        Ok(Self {
            id: ModuleId::try_from(manifest.id.clone())?,
            name: manifest.name.clone(),
            service,
            manifest_sha256: digest(&SHA256, original)
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            bootstrap,
            models,
        })
    }
}

fn safe_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && value.split('/').all(valid_segment)
        && !value.contains('\\')
}

fn valid_revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

fn valid_language(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 24
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Unknown(u32),
    NotApplicable,
}

impl HardwareVendor {
    pub const fn from_pci_vendor_id(vendor_id: u32) -> Self {
        match vendor_id {
            0x10de => Self::Nvidia,
            0x1002 => Self::Amd,
            0x8086 => Self::Intel,
            0x106b => Self::Apple,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    pub id: HardwareId,
    pub vendor: HardwareVendor,
    pub architecture: Option<String>,
    pub dedicated_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct HardwareInventory {
    devices: Vec<Hardware>,
}

impl HardwareInventory {
    pub fn new(devices: Vec<Hardware>) -> Result<Self, ModuleContractError> {
        let mut seen = HashSet::new();
        if !devices.iter().all(|device| seen.insert(device.id.clone())) {
            return Err(ModuleContractError::InvalidManifest);
        }
        Ok(Self { devices })
    }

    pub fn from_system(
        source: &super::system_hardware::SystemHardware,
    ) -> Result<Self, ModuleContractError> {
        let mut devices = vec![Hardware {
            id: HardwareId::try_from("cpu_0".to_owned())?,
            vendor: HardwareVendor::NotApplicable,
            architecture: None,
            dedicated_memory_bytes: None,
        }];
        for adapter in &source.adapters {
            let luid = adapter
                .luid
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            devices.push(Hardware {
                id: HardwareId::try_from(format!("gpu_{luid}"))?,
                vendor: HardwareVendor::from_pci_vendor_id(adapter.vendor_id),
                architecture: None,
                dedicated_memory_bytes: Some(adapter.dedicated_memory_bytes),
            });
        }
        Self::new(devices)
    }

    pub fn devices(&self) -> &[Hardware] {
        &self.devices
    }

    pub fn require(&self, id: &HardwareId) -> Result<&Hardware, ModuleContractError> {
        self.devices
            .iter()
            .find(|device| &device.id == id)
            .ok_or(ModuleContractError::UnknownHardware)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionVariant {
    pub id: ExecutionVariantId,
    pub runtime: String,
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionConfiguration {
    pub module: ModuleId,
    pub model: ModelId,
    pub hardware: HardwareId,
    pub variant: ExecutionVariantId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration(json: &str) -> Result<ModelModuleDeclaration, ModuleContractError> {
        let manifest: PythonPluginManifest = serde_json::from_str(json).unwrap();
        ModelModuleDeclaration::from_python_manifest(&manifest, json.as_bytes())
    }

    #[test]
    fn offline_declaration_has_only_bootstrap_and_declared_models() {
        let raw = r#"{"id":"mock_tts","name":"Mock","version":"0.1.0","kind":"tts","entrypoint":"plugin.py","api_version":1,"models":[{"id":"first","revision":"1.0.0","label":"Same"},{"id":"second","revision":"2.0.0","label":"Same"}]}"#;
        let module = declaration(raw).unwrap();
        assert_eq!(module.models.len(), 2);
        assert_ne!(module.models[0].id, module.models[1].id);
        assert_eq!(module.bootstrap, ModuleBootstrap::python_sdk_312());
        assert_eq!(module.manifest_sha256.len(), 64);
        assert_eq!(module.service, AiService::Tts);
    }

    #[test]
    fn bootstrap_cannot_predeclare_inference_dependencies_or_change_host() {
        let invalid = r#"{"id":"mock_tts","name":"Mock","kind":"tts","entrypoint":"plugin.py","api_version":1,"bootstrap":{"kind":"python_sdk","python_minor":"3.13","sdk_api_version":1}}"#;
        assert_eq!(
            declaration(invalid).unwrap_err(),
            ModuleContractError::InvalidBootstrap
        );
        let duplicate = r#"{"id":"mock_tts","name":"Mock","kind":"tts","entrypoint":"plugin.py","api_version":1,"models":[{"id":"one","revision":"1","label":"one"},{"id":"one","revision":"2","label":"two"}]}"#;
        assert_eq!(
            declaration(duplicate).unwrap_err(),
            ModuleContractError::InvalidModel
        );
    }

    #[test]
    fn one_physical_gpu_remains_one_hardware_even_with_cuda_and_dxgi() {
        use super::super::system_hardware::{CudaDevice, SystemAdapter, SystemHardware};
        let adapter = SystemAdapter {
            name: "GPU".into(),
            vendor_id: 0x10de,
            dedicated_memory_bytes: 16 * 1024 * 1024 * 1024,
            luid: [1, 2, 3, 4, 5, 6, 7, 8],
            cuda: Some(CudaDevice {
                ordinal: 4,
                major: 9,
                minor: 0,
            }),
        };
        let a = HardwareInventory::from_system(&SystemHardware {
            adapters: vec![adapter.clone()],
            errors: Vec::new(),
        })
        .unwrap();
        let mut another = adapter;
        another.cuda.as_mut().unwrap().ordinal = 0;
        let b = HardwareInventory::from_system(&SystemHardware {
            adapters: vec![another],
            errors: Vec::new(),
        })
        .unwrap();
        assert_eq!(a.devices().len(), 2);
        assert_eq!(a.devices()[1].id, b.devices()[1].id);
        assert_eq!(a.devices()[1].vendor, HardwareVendor::Nvidia);
    }

    #[test]
    fn hardware_lookup_does_not_infer_runtime_from_vendor() {
        let hardware = Hardware {
            id: HardwareId::try_from("pci_0000_01_00_0".to_owned()).unwrap(),
            vendor: HardwareVendor::Nvidia,
            architecture: None,
            dedicated_memory_bytes: None,
        };
        let inventory = HardwareInventory::new(vec![hardware.clone()]).unwrap();
        assert_eq!(inventory.require(&hardware.id).unwrap(), &hardware);
        assert_eq!(
            inventory.require(&HardwareId::try_from("cpu_0".to_owned()).unwrap()),
            Err(ModuleContractError::UnknownHardware)
        );
        assert!(HardwareInventory::new(vec![hardware.clone(), hardware]).is_err());
    }
}
