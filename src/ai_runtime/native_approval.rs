//! Publisher-authorized native package admission independent of local file discovery.
//! Only an externally pinned Ed25519 public key can approve a manifest digest.

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::native_registry::{NativeRegistryError, parse_sha256, valid_identifier};

const SCHEMA_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 4096;
const MAX_PUBLISHERS: usize = 32;

#[derive(Debug, Error)]
pub enum NativeApprovalError {
    #[error("invalid native publisher trust policy: {0}")]
    InvalidPolicy(&'static str),
    #[error("invalid native publisher approval: {0}")]
    InvalidApproval(&'static str),
    #[error("native publisher is not trusted")]
    UnknownPublisher,
    #[error("invalid native publisher signature")]
    InvalidSignature,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
}

/// A key supplied by an independent application trust decision, not by the
/// downloaded model or its manifest. This structure contains no private key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePublisherKey {
    pub publisher_id: String,
    pub ed25519_public_key: [u8; 32],
}

/// An untrusted wire document. Its fields are not an approval until verified.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedNativeRelease {
    pub schema_version: u32,
    pub publisher_id: String,
    pub model_id: String,
    pub manifest_sha256: String,
    pub signature_base64: String,
}

/// This value can only be constructed after checking a pinned publisher key.
/// It proves publisher authorization of a manifest digest, NOT file integrity
/// or a running worker. The installer must still verify the manifest and files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedNativeRelease {
    publisher_id: String,
    model_id: String,
    manifest_sha256: [u8; 32],
}

impl VerifiedNativeRelease {
    pub fn publisher_id(&self) -> &str {
        &self.publisher_id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn manifest_sha256(&self) -> [u8; 32] {
        self.manifest_sha256
    }
}

/// Pure, bounded verifier. Key enrollment and persistence are deliberately NOT
/// performed here; the caller must pin the public keys independently.
#[derive(Debug, Clone)]
pub struct NativePublisherVerifier {
    keys: BTreeMap<String, [u8; 32]>,
}

impl NativePublisherVerifier {
    pub fn new(
        keys: impl IntoIterator<Item = NativePublisherKey>,
    ) -> Result<Self, NativeApprovalError> {
        let mut trusted = BTreeMap::new();
        for key in keys {
            if !valid_publisher_id(&key.publisher_id) {
                return Err(NativeApprovalError::InvalidPolicy("invalid publisher id"));
            }
            if trusted.len() == MAX_PUBLISHERS {
                return Err(NativeApprovalError::InvalidPolicy(
                    "too many publisher keys",
                ));
            }
            if trusted
                .insert(key.publisher_id, key.ed25519_public_key)
                .is_some()
            {
                return Err(NativeApprovalError::InvalidPolicy("duplicate publisher id"));
            }
        }
        Ok(Self { keys: trusted })
    }

    /// Verifies a detached publisher signature without exposing trusted key material.
    /// This is shared with the independently domain-separated release-index contract.
    pub(super) fn verify_detached(
        &self,
        publisher_id: &str,
        message: &[u8],
        signature_base64: &str,
    ) -> Result<(), NativeApprovalError> {
        if !valid_publisher_id(publisher_id) {
            return Err(NativeApprovalError::InvalidApproval("publisher identifier"));
        }
        let signature = STANDARD
            .decode(signature_base64.as_bytes())
            .map_err(|_| NativeApprovalError::InvalidApproval("signature encoding"))?;
        if signature.len() != 64 || STANDARD.encode(&signature) != signature_base64 {
            return Err(NativeApprovalError::InvalidApproval(
                "signature canonicality",
            ));
        }
        let key = self
            .keys
            .get(publisher_id)
            .ok_or(NativeApprovalError::UnknownPublisher)?;
        UnparsedPublicKey::new(&ED25519, key)
            .verify(message, &signature)
            .map_err(|_| NativeApprovalError::InvalidSignature)
    }

    /// Parses one externally issued approval. There is no default key, trust-on-
    /// first-use behavior, or acceptance of a key embedded in the document.
    pub fn verify(&self, bytes: &[u8]) -> Result<VerifiedNativeRelease, NativeApprovalError> {
        if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(NativeApprovalError::InvalidApproval("approval size"));
        }
        let document: SignedNativeRelease = serde_json::from_slice(bytes)?;
        if document.schema_version != SCHEMA_VERSION
            || !valid_publisher_id(&document.publisher_id)
            || !valid_identifier(&document.model_id)
        {
            return Err(NativeApprovalError::InvalidApproval(
                "approval identity or schema",
            ));
        }
        let digest = parse_sha256(&document.manifest_sha256)?;
        if document.manifest_sha256 != hex_digest(&digest) {
            return Err(NativeApprovalError::InvalidApproval("noncanonical digest"));
        }
        let signature = STANDARD
            .decode(document.signature_base64.as_bytes())
            .map_err(|_| NativeApprovalError::InvalidApproval("signature encoding"))?;
        if signature.len() != 64 || STANDARD.encode(&signature) != document.signature_base64 {
            return Err(NativeApprovalError::InvalidApproval(
                "signature size or encoding",
            ));
        }
        let public_key = self
            .keys
            .get(&document.publisher_id)
            .ok_or(NativeApprovalError::UnknownPublisher)?;
        let message = signing_message(
            &document.publisher_id,
            &document.model_id,
            &document.manifest_sha256,
        );
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(message.as_bytes(), &signature)
            .map_err(|_| NativeApprovalError::InvalidSignature)?;
        Ok(VerifiedNativeRelease {
            publisher_id: document.publisher_id,
            model_id: document.model_id,
            manifest_sha256: digest,
        })
    }
}

/// The exact UTF-8 preimage that an external publisher must sign with Ed25519.
/// Newline injection is impossible because both identifiers are restricted ASCII.
/// `manifest_sha256` is exactly 64 lowercase hexadecimal characters.
pub fn signing_message(publisher_id: &str, model_id: &str, manifest_sha256: &str) -> String {
    format!("neuromita.native-release.v1\n{publisher_id}\n{model_id}\n{manifest_sha256}\n")
}

pub(super) fn valid_publisher_id(value: &str) -> bool {
    valid_identifier(value) && value.len() <= 64
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
