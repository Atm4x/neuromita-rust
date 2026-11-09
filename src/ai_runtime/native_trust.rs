//! Independent publisher trust policy for native releases. Package data cannot enroll keys.
//! A missing application policy or signed release denies admission, never enables TOFU.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use thiserror::Error;

use super::{
    native_approval::{
        NativeApprovalError, NativePublisherKey, NativePublisherVerifier, VerifiedNativeRelease,
    },
    native_registry::{NativeRegistryError, check_path, valid_identifier},
};

const MAX_TRUST_BYTES: u64 = 16 * 1024;
const MAX_APPROVAL_BYTES: u64 = 4096;

#[derive(Debug, Error)]
pub enum NativeTrustError {
    #[error("publisher has not authorized this installed native revision")]
    NotAuthorized,
    #[error("signed publisher release does not match the requested revision")]
    ReleaseMismatch,
    #[error("invalid application-owned native publisher policy: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Approval(#[from] NativeApprovalError),
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeTrustVerdict {
    Authorized { publisher_id: String },
    Missing,
    Rejected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustDocument {
    schema_version: u32,
    publishers: Vec<TrustedPublisherDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustedPublisherDocument {
    publisher_id: String,
    ed25519_public_key_base64: String,
}

/// The trust-policy file must be provisioned by the application administrator,
/// independently of any model package or downloaded release document. This
/// component never writes/creates it and never enrolls a key implicitly.
/// Signed approvals are untrusted inputs, verified on every selection/activation.
#[derive(Debug, Clone)]
pub struct NativeReleaseTrust {
    policy_file: PathBuf,
    approvals_root: PathBuf,
}

impl NativeReleaseTrust {
    pub fn new(policy_file: PathBuf, approvals_root: PathBuf) -> Self {
        Self {
            policy_file,
            approvals_root,
        }
    }

    pub fn verify(
        &self,
        model_id: &str,
        digest: &[u8; 32],
    ) -> Result<VerifiedNativeRelease, NativeTrustError> {
        if !valid_identifier(model_id) {
            return Err(NativeTrustError::Invalid("invalid model identifier"));
        }
        let verifier = self.load_verifier()?;
        let path = self
            .approvals_root
            .join(model_id)
            .join(format!("{}.json", hex_digest(digest)));
        let bytes =
            read_optional(&path, MAX_APPROVAL_BYTES)?.ok_or(NativeTrustError::NotAuthorized)?;
        let approved = verifier.verify(&bytes)?;
        if approved.model_id() != model_id || approved.manifest_sha256() != *digest {
            return Err(NativeTrustError::ReleaseMismatch);
        }
        Ok(approved)
    }

    /// Metadata-only trust indication for UI. This never hashes weights and never
    /// equates publisher approval with installed-artifact integrity or worker Ready.
    pub fn verdict(&self, model_id: &str, digest: &[u8; 32]) -> NativeTrustVerdict {
        match self.verify(model_id, digest) {
            Ok(approved) => NativeTrustVerdict::Authorized {
                publisher_id: approved.publisher_id().to_owned(),
            },
            Err(NativeTrustError::NotAuthorized) => NativeTrustVerdict::Missing,
            Err(_) => NativeTrustVerdict::Rejected,
        }
    }

    fn load_verifier(&self) -> Result<NativePublisherVerifier, NativeTrustError> {
        let Some(bytes) = read_optional(&self.policy_file, MAX_TRUST_BYTES)? else {
            return Ok(NativePublisherVerifier::new([])?);
        };
        let document: TrustDocument = serde_json::from_slice(&bytes)?;
        if document.schema_version != 1 || document.publishers.len() > 32 {
            return Err(NativeTrustError::Invalid(
                "schema version or number of keys",
            ));
        }
        let keys = document
            .publishers
            .into_iter()
            .map(|publisher| {
                let decoded = STANDARD
                    .decode(publisher.ed25519_public_key_base64.as_bytes())
                    .map_err(|_| NativeTrustError::Invalid("public key encoding"))?;
                if decoded.len() != 32
                    || STANDARD.encode(&decoded) != publisher.ed25519_public_key_base64
                {
                    return Err(NativeTrustError::Invalid(
                        "public key length or canonicality",
                    ));
                }
                let mut key = [0u8; 32];
                key.copy_from_slice(&decoded);
                Ok(NativePublisherKey {
                    publisher_id: publisher.publisher_id,
                    ed25519_public_key: key,
                })
            })
            .collect::<Result<Vec<_>, NativeTrustError>>()?;
        Ok(NativePublisherVerifier::new(keys)?)
    }
}

fn hex_digest(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(64);
    for b in digest {
        write!(&mut text, "{b:02x}").expect("writing to String cannot fail");
    }
    text
}

fn read_optional(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, NativeTrustError> {
    if !path.is_absolute() {
        return Err(NativeTrustError::Invalid("trust paths must be absolute"));
    }
    let mut ancestors = path.ancestors().skip(1).collect::<Vec<_>>();
    ancestors.reverse();
    for parent in ancestors {
        match fs::symlink_metadata(parent) {
            Ok(_) => check_path(parent, true)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    match fs::symlink_metadata(path) {
        Ok(_) => check_path(path, false)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 || len > limit {
        return Err(NativeTrustError::Invalid("trust file size"));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != len {
        return Err(NativeTrustError::Invalid(
            "trust file changed while reading",
        ));
    }
    Ok(Some(bytes))
}
