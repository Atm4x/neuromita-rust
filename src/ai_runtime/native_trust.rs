//! Independent publisher trust policy for native releases. Package data cannot enroll keys.
//! A missing application policy or signed release denies admission, never enables TOFU.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use thiserror::Error;

use super::{
    native_approval::{
        NativeApprovalError, NativePublisherKey, NativePublisherVerifier, VerifiedNativeRelease,
    },
    native_registry::{NativeRegistryError, check_path, parse_sha256, valid_identifier},
};

const MAX_TRUST_BYTES: u64 = 16 * 1024;
const MAX_APPROVAL_BYTES: u64 = 4096;
const MAX_APPROVAL_MODELS: usize = 128;
const MAX_APPROVED_RELEASES: usize = 256;

/// A signed, publisher-authorized release identity, not evidence that its
/// manifest or model files are installed, intact, or supported by the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAuthorizedRelease {
    pub model_id: String,
    pub manifest_sha256: [u8; 32],
    pub publisher_id: String,
}

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

/// Binds an exact publisher policy to the trusted application build. A local
/// JSON file is never a trust anchor by itself, even if its keys are well-formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePolicyPin([u8; 32]);

impl NativePolicyPin {
    pub fn from_sha256_hex(value: &str) -> Result<Self, NativeTrustError> {
        let digest = parse_sha256(value)?;
        if value != hex_digest(&digest) {
            return Err(NativeTrustError::Invalid(
                "policy pin must be lowercase SHA-256",
            ));
        }
        Ok(Self(digest))
    }
}

/// A publisher policy is accepted only if its exact bytes match an independently
/// pinned digest. `None` is a deliberate deny-all state for builds without a
/// provisioned anchor. Signed release documents are verified on every use.
#[derive(Debug, Clone)]
pub struct NativeReleaseTrust {
    policy_file: PathBuf,
    approvals_root: PathBuf,
    policy_pin: Option<NativePolicyPin>,
}

impl NativeReleaseTrust {
    pub fn new(
        policy_file: PathBuf,
        approvals_root: PathBuf,
        policy_pin: Option<NativePolicyPin>,
    ) -> Self {
        Self {
            policy_file,
            approvals_root,
            policy_pin,
        }
    }

    /// Treats a user-supplied digest only as a lookup key, never as approval.
    pub fn verify_hex(
        &self,
        model_id: &str,
        digest_hex: &str,
    ) -> Result<VerifiedNativeRelease, NativeTrustError> {
        self.verify(model_id, &parse_sha256(digest_hex)?)
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

    /// A bounded release catalog derived solely from verified signed approvals.
    /// There is no mutable catalog JSON, network data, trust-on-first-use key or
    /// fallback to manifest-discovered digests. Invalid entries fail closed.
    pub fn available_releases(&self) -> Result<Vec<NativeAuthorizedRelease>, NativeTrustError> {
        let verifier = self.load_verifier()?;
        let mut releases = Vec::new();
        let models = read_directory(&self.approvals_root)?;
        if models.len() > MAX_APPROVAL_MODELS {
            return Err(NativeTrustError::Invalid("too many approved models"));
        }
        for (model_id, model_path) in models {
            if !valid_identifier(&model_id) {
                return Err(NativeTrustError::Invalid(
                    "invalid approved model directory",
                ));
            }
            check_path(&model_path, true)?;
            let documents = read_directory(&model_path)?;
            if documents.len() > MAX_APPROVED_RELEASES.saturating_sub(releases.len()) {
                return Err(NativeTrustError::Invalid("too many approved releases"));
            }
            for (name, path) in documents {
                let digest_hex = name
                    .strip_suffix(".json")
                    .ok_or(NativeTrustError::Invalid("unexpected approval entry"))?;
                let expected = parse_sha256(digest_hex)?;
                if hex_digest(&expected) != digest_hex {
                    return Err(NativeTrustError::Invalid("noncanonical approval filename"));
                }
                let bytes = read_optional(&path, MAX_APPROVAL_BYTES)?
                    .ok_or(NativeTrustError::Invalid("approved release disappeared"))?;
                let approved = verifier.verify(&bytes)?;
                if approved.model_id() != model_id || approved.manifest_sha256() != expected {
                    return Err(NativeTrustError::ReleaseMismatch);
                }
                releases.push(NativeAuthorizedRelease {
                    model_id: model_id.clone(),
                    manifest_sha256: expected,
                    publisher_id: approved.publisher_id().to_owned(),
                });
            }
        }
        releases.sort_by(|a, b| {
            a.model_id
                .cmp(&b.model_id)
                .then(a.manifest_sha256.cmp(&b.manifest_sha256))
        });
        Ok(releases)
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
        let pin = self.policy_pin.ok_or(NativeTrustError::NotAuthorized)?;
        let bytes = read_optional(&self.policy_file, MAX_TRUST_BYTES)?
            .ok_or(NativeTrustError::NotAuthorized)?;
        if digest(&SHA256, &bytes).as_ref() != pin.0 {
            return Err(NativeTrustError::Invalid(
                "publisher policy does not match binary-pinned SHA-256",
            ));
        }
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

/// Check every ancestor before enumeration, including a missing root. We do not
/// create a directory or follow a symlink supplied by a release publisher.
fn read_directory(path: &Path) -> Result<Vec<(String, PathBuf)>, NativeTrustError> {
    if !path.is_absolute() {
        return Err(NativeTrustError::Invalid("approval root must be absolute"));
    }
    let mut ancestors = path.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for parent in ancestors {
        match fs::symlink_metadata(parent) {
            Ok(_) => check_path(parent, true)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| NativeTrustError::Invalid("non-UTF8 approval entry"))?;
        entries.push((name, entry.path()));
        if entries.len() > MAX_APPROVED_RELEASES {
            return Err(NativeTrustError::Invalid("too many approval entries"));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}
