use super::{PresetCatalogError, PresetCredentials};
use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", content = "name", rename_all = "snake_case")]
pub enum CredentialReference {
    Environment(String),
    Local(String),
}

impl CredentialReference {
    pub fn parse(value: &str) -> Result<Self, PresetCatalogError> {
        let reference = match value.split_once(':') {
            Some(("env", name)) => Self::Environment(name.into()),
            Some(("local", name)) => Self::Local(name.into()),
            _ => {
                return Err(PresetCatalogError::Invalid(
                    "credential reference requires env: or local:".into(),
                ));
            }
        };
        reference.validate()?;
        Ok(reference)
    }
    pub fn validate(&self) -> Result<(), PresetCatalogError> {
        let name = match self {
            Self::Environment(name) | Self::Local(name) => name,
        };
        if name.is_empty()
            || name.len() > 128
            || name.as_bytes()[0].is_ascii_digit()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(PresetCatalogError::Invalid(
                "invalid credential reference name".into(),
            ));
        }
        Ok(())
    }
}
impl From<String> for CredentialReference {
    fn from(name: String) -> Self {
        Self::Environment(name)
    }
}
impl From<&str> for CredentialReference {
    fn from(name: &str) -> Self {
        Self::Environment(name.into())
    }
}
impl fmt::Display for CredentialReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment(name) => write!(f, "env:{name}"),
            Self::Local(name) => write!(f, "local:{name}"),
        }
    }
}

pub struct ApplicationPresetCredentials {
    path: PathBuf,
}
impl ApplicationPresetCredentials {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}
impl PresetCredentials for ApplicationPresetCredentials {
    fn get(&self, name: &str) -> Result<String, PresetCatalogError> {
        super::EnvironmentPresetCredentials.get(name)
    }
    fn resolve(&self, reference: &CredentialReference) -> Result<String, PresetCatalogError> {
        reference.validate()?;
        match reference {
            CredentialReference::Environment(name) => self.get(name),
            CredentialReference::Local(name) => {
                let bytes = std::fs::read(&self.path).map_err(|_| {
                    PresetCatalogError::Storage("local credential store cannot be read".into())
                })?;
                if bytes.len() > 1_048_576 {
                    return Err(PresetCatalogError::Storage(
                        "local credential store exceeds bounds".into(),
                    ));
                }
                let values: std::collections::BTreeMap<String, String> =
                    serde_json::from_slice(&bytes).map_err(|_| {
                        PresetCatalogError::Storage("invalid local credential store".into())
                    })?;
                values
                    .get(name)
                    .filter(|value| !value.is_empty() && value.len() <= 16_384)
                    .cloned()
                    .ok_or_else(|| PresetCatalogError::MissingCredential(reference.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_references_are_explicit_and_do_not_fall_back_to_environment() {
        let path =
            std::env::temp_dir().join(format!("preset-secrets-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, r#"{"imported_18":"fixture-secret"}"#).unwrap();
        let provider = ApplicationPresetCredentials::new(path.clone());
        let reference = CredentialReference::parse("local:imported_18").unwrap();
        assert_eq!(provider.resolve(&reference).unwrap(), "fixture-secret");
        assert!(
            provider
                .resolve(&CredentialReference::Local("missing".into()))
                .is_err()
        );
        assert!(CredentialReference::parse("imported_18").is_err());
        assert_eq!(
            serde_json::to_value(reference).unwrap(),
            serde_json::json!({"source":"local","name":"imported_18"})
        );
        std::fs::remove_file(path).unwrap();
    }
}
