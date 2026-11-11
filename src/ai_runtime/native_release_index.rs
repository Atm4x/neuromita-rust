//! Signed, bounded publisher index transport. Release approval remains the sole
//! Core source for UI catalog and download admission; an index is never trusted
//! merely because it came from HTTPS.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, Url, header::ACCEPT_ENCODING, redirect::Policy};
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    native_approval::{
        NativeApprovalError, NativePublisherVerifier, SignedNativeRelease, valid_publisher_id,
    },
    native_https::NativeHttpsOrigin,
    native_package_install::{ManagedLock, NativePackageInstallError, sync_dir},
    native_registry::{NativeRegistryError, check_path, parse_sha256, valid_identifier},
    native_trust::{NativeReleaseTrust, NativeTrustError},
};

const MAX_INDEX_BYTES: usize = 256 * 1024;
const MAX_INDEX_RELEASES: usize = 256;
const MAX_RECORD_BYTES: u64 = 512;
const MAX_APPROVAL_BYTES: usize = 4096;
const MAX_EXPIRY_HORIZON: u64 = 180 * 24 * 60 * 60;

#[derive(Debug, Error)]
pub enum NativeIndexError {
    #[error("invalid publisher index: {0}")]
    Invalid(&'static str),
    #[error("publisher index is expired")]
    Expired,
    #[error("publisher index generation rolled back or equivocated")]
    Rollback,
    #[error("publisher index refresh canceled")]
    Canceled,
    #[error("remote publisher index did not return HTTP 200")]
    HttpStatus,
    #[error(transparent)]
    Approval(#[from] NativeApprovalError),
    #[error(transparent)]
    Trust(#[from] NativeTrustError),
    #[error(transparent)]
    Install(#[from] NativePackageInstallError),
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("publisher index persistence task failed")]
    Task,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedNativeReleaseIndex {
    pub schema_version: u32,
    pub publisher_id: String,
    pub generation: u64,
    pub expires_unix_secs: u64,
    pub releases: Vec<SignedNativeRelease>,
    pub signature_base64: String,
}

/// Canonical, domain-separated signing preimage for external index publishers.
/// All components are constrained ASCII; no JSON field order is signed implicitly.
pub fn index_signing_message(index: &SignedNativeReleaseIndex) -> String {
    let mut message = format!(
        "neuromita.native-index.v1\n{}\n{}\n{}\n{}\n",
        index.publisher_id,
        index.generation,
        index.expires_unix_secs,
        index.releases.len()
    );
    for release in &index.releases {
        message.push_str(&format!(
            "{}\n{}\n{}\n",
            release.model_id, release.manifest_sha256, release.signature_base64
        ));
    }
    message
}

#[derive(Debug, Clone)]
pub struct VerifiedNativeReleaseIndex {
    publisher_id: String,
    generation: u64,
    index_sha256: [u8; 32],
    release_documents: Vec<(String, [u8; 32], Vec<u8>)>,
}

impl VerifiedNativeReleaseIndex {
    pub fn publisher_id(&self) -> &str {
        &self.publisher_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn release_count(&self) -> usize {
        self.release_documents.len()
    }
}

/// Explicit validation with an independently provisioned, binary-pinned key policy.
/// The verified index may update approved documents but never selects a model.
pub fn verify_index(
    bytes: &[u8],
    verifier: &NativePublisherVerifier,
    expected_publisher: &str,
    now_unix_secs: u64,
) -> Result<VerifiedNativeReleaseIndex, NativeIndexError> {
    if bytes.is_empty() || bytes.len() > MAX_INDEX_BYTES {
        return Err(NativeIndexError::Invalid("index size"));
    }
    let index: SignedNativeReleaseIndex = serde_json::from_slice(bytes)?;
    if index.schema_version != 1
        || !valid_publisher_id(&index.publisher_id)
        || index.publisher_id != expected_publisher
        || index.generation == 0
        || index.releases.is_empty()
        || index.releases.len() > MAX_INDEX_RELEASES
    {
        return Err(NativeIndexError::Invalid(
            "index identity, sequence or count",
        ));
    }
    if index.expires_unix_secs <= now_unix_secs {
        return Err(NativeIndexError::Expired);
    }
    if index.expires_unix_secs - now_unix_secs > MAX_EXPIRY_HORIZON {
        return Err(NativeIndexError::Invalid("expiry horizon"));
    }
    let mut identities = BTreeSet::new();
    let mut documents = Vec::with_capacity(index.releases.len());
    for release in &index.releases {
        if release.schema_version != 1
            || release.publisher_id != index.publisher_id
            || !valid_identifier(&release.model_id)
        {
            return Err(NativeIndexError::Invalid("release identity"));
        }
        let digest = parse_sha256(&release.manifest_sha256)?;
        if release.manifest_sha256 != hex_digest(&digest) {
            return Err(NativeIndexError::Invalid("noncanonical release digest"));
        }
        let identity = (release.model_id.clone(), digest);
        if !identities.insert(identity.clone()) {
            return Err(NativeIndexError::Invalid("duplicate release identity"));
        }
        let raw = serde_json::to_vec(release)?;
        if raw.len() > MAX_APPROVAL_BYTES {
            return Err(NativeIndexError::Invalid("release document size"));
        }
        let checked = verifier.verify(&raw)?;
        if checked.model_id() != release.model_id
            || checked.manifest_sha256() != digest
            || checked.publisher_id() != index.publisher_id
        {
            return Err(NativeIndexError::Invalid("release approval mismatch"));
        }
        documents.push((identity.0, digest, raw));
    }
    let message = index_signing_message(&index);
    verifier.verify_detached(
        &index.publisher_id,
        message.as_bytes(),
        &index.signature_base64,
    )?;
    let mut sha = [0; 32];
    sha.copy_from_slice(digest(&SHA256, bytes).as_ref());
    Ok(VerifiedNativeReleaseIndex {
        publisher_id: index.publisher_id,
        generation: index.generation,
        index_sha256: sha,
        release_documents: documents,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexReceipt {
    schema_version: u32,
    publisher_id: String,
    generation: u64,
    index_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeIndexSyncResult {
    pub publisher_id: String,
    pub generation: u64,
    pub approvals_written: usize,
    pub replayed: bool,
}

/// State root must be an existing private app directory, disjoint from model
/// package storage. The high-water receipt is committed LAST after all individually
/// verified approvals, preventing a crash from recording a sequence without its files.
#[derive(Debug, Clone)]
pub struct NativeReleaseIndexStore {
    root: PathBuf,
}

impl NativeReleaseIndexStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn accept(
        &self,
        trust: &NativeReleaseTrust,
        index_bytes: &[u8],
        publisher_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<NativeIndexSyncResult, NativeIndexError> {
        if cancellation.is_cancelled() {
            return Err(NativeIndexError::Canceled);
        }
        check_path(&self.root, true)?;
        check_path(trust.approval_directory(), true)?;
        let _owner = ManagedLock::acquire(&self.root)?;
        let now = unix_now()?;
        let index = verify_index(index_bytes, &trust.load_verifier()?, publisher_id, now)?;
        let receipt_path = self.root.join(format!("{publisher_id}.json"));
        let current = read_receipt(&receipt_path, publisher_id)?;
        if let Some(ref receipt) = current {
            if index.generation < receipt.generation
                || (index.generation == receipt.generation
                    && index.index_sha256 != parse_sha256(&receipt.index_sha256)?)
            {
                return Err(NativeIndexError::Rollback);
            }
        }
        let replayed = current.is_some_and(|r| r.generation == index.generation);
        let mut authorized = trust.available_releases()?;
        let mut unique = authorized
            .drain(..)
            .map(|release| (release.model_id, release.manifest_sha256))
            .collect::<BTreeSet<_>>();
        for (model_id, digest, _) in &index.release_documents {
            unique.insert((model_id.clone(), *digest));
        }
        if unique.len() > MAX_INDEX_RELEASES
            || unique
                .iter()
                .map(|entry| &entry.0)
                .collect::<BTreeSet<_>>()
                .len()
                > 128
        {
            return Err(NativeIndexError::Invalid("approval catalog capacity"));
        }
        // Recheck the pinned policy within the publication lock, not just when
        // the network request started. Never accept keys from this index.
        let _ = verify_index(
            index_bytes,
            &trust.load_verifier()?,
            publisher_id,
            unix_now()?,
        )?;
        let mut inserted = 0;
        for (model_id, digest, approval_bytes) in &index.release_documents {
            if cancellation.is_cancelled() {
                return Err(NativeIndexError::Canceled);
            }
            let model_dir = trust.approval_directory().join(model_id);
            if !model_dir.exists() {
                fs::create_dir(&model_dir)?;
                sync_dir(trust.approval_directory())?;
            }
            check_path(&model_dir, true)?;
            let target = model_dir.join(format!("{}.json", hex_digest(digest)));
            match fs::symlink_metadata(&target) {
                Ok(_) => {
                    check_path(&target, false)?;
                    let existing = read_bounded(&target, MAX_APPROVAL_BYTES)?;
                    if existing != *approval_bytes {
                        // A preexisting signed release is never silently replaced.
                        return Err(NativeIndexError::Invalid("approval conflict"));
                    }
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
            let temp = model_dir.join(format!(".incoming-{}", Uuid::new_v4()));
            let result = (|| -> Result<(), NativeIndexError> {
                let mut out = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp)?;
                out.write_all(approval_bytes)?;
                out.sync_all()?;
                fs::rename(&temp, &target)?;
                sync_dir(&model_dir)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temp);
            }
            result?;
            inserted += 1;
        }
        if cancellation.is_cancelled() {
            return Err(NativeIndexError::Canceled);
        }
        let receipt = IndexReceipt {
            schema_version: 1,
            publisher_id: publisher_id.into(),
            generation: index.generation,
            index_sha256: hex_digest(&index.index_sha256),
        };
        let record = serde_json::to_vec(&receipt)?;
        if record.len() as u64 > MAX_RECORD_BYTES {
            return Err(NativeIndexError::Invalid("receipt size"));
        }
        let temp = self.root.join(format!(".receipt-{}", Uuid::new_v4()));
        let result = (|| -> Result<(), NativeIndexError> {
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            out.write_all(&record)?;
            out.sync_all()?;
            fs::rename(&temp, &receipt_path)?;
            sync_dir(&self.root)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result?;
        Ok(NativeIndexSyncResult {
            publisher_id: publisher_id.into(),
            generation: index.generation,
            approvals_written: inserted,
            replayed,
        })
    }
}

/// Fetches one fixed publisher index from an independently supplied HTTPS origin.
/// The URL cannot be set or redirected by a model or by the signed document.
pub struct NativeReleaseIndexTransfer {
    client: Client,
    file_timeout: Duration,
}

impl NativeReleaseIndexTransfer {
    pub fn new(file_timeout: Duration) -> Result<Self, NativeIndexError> {
        if file_timeout.is_zero() || file_timeout > Duration::from_secs(300) {
            return Err(NativeIndexError::Invalid("index timeout"));
        }
        Ok(Self {
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(file_timeout)
                .build()?,
            file_timeout,
        })
    }

    pub async fn refresh(
        &self,
        origin: &NativeHttpsOrigin,
        publisher_id: &str,
        trust: NativeReleaseTrust,
        store: NativeReleaseIndexStore,
        cancellation: &CancellationToken,
    ) -> Result<NativeIndexSyncResult, NativeIndexError> {
        self.refresh_from_url(origin.url(), publisher_id, trust, store, cancellation)
            .await
    }

    async fn refresh_from_url(
        &self,
        origin: &Url,
        publisher_id: &str,
        trust: NativeReleaseTrust,
        store: NativeReleaseIndexStore,
        cancellation: &CancellationToken,
    ) -> Result<NativeIndexSyncResult, NativeIndexError> {
        if !valid_publisher_id(publisher_id) {
            return Err(NativeIndexError::Invalid("publisher ID"));
        }
        // Fail before network I/O when the build has no independent trust root.
        let _ = trust.load_verifier()?;
        if cancellation.is_cancelled() {
            return Err(NativeIndexError::Canceled);
        }
        let mut url = origin.clone();
        url.path_segments_mut()
            .map_err(|_| NativeIndexError::Invalid("index URL"))?
            .pop_if_empty()
            .push("indexes")
            .push(publisher_id)
            .push("index.json");
        let future = async {
            let mut response = self
                .client
                .get(url)
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(NativeIndexError::HttpStatus);
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_INDEX_BYTES as u64)
            {
                return Err(NativeIndexError::Invalid("index response too large"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len().saturating_add(chunk.len()) > MAX_INDEX_BYTES {
                    return Err(NativeIndexError::Invalid("index response too large"));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        };
        let bytes = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(NativeIndexError::Canceled),
            result = tokio::time::timeout(self.file_timeout, future) => {
                result.map_err(|_| NativeIndexError::Invalid("index timeout"))??
            }
        };
        let publisher_id = publisher_id.to_string();
        let cancellation_owned = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            store.accept(&trust, &bytes, &publisher_id, &cancellation_owned)
        })
        .await
        .map_err(|_| NativeIndexError::Task)?
    }
}

fn read_receipt(path: &Path, expected: &str) -> Result<Option<IndexReceipt>, NativeIndexError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => check_path(path, false)?,
    }
    let record: IndexReceipt =
        serde_json::from_slice(&read_bounded(path, MAX_RECORD_BYTES as usize)?)?;
    if record.schema_version != 1 || record.publisher_id != expected || record.generation == 0 {
        return Err(NativeIndexError::Invalid("receipt identity"));
    }
    let parsed = parse_sha256(&record.index_sha256)?;
    if hex_digest(&parsed) != record.index_sha256 {
        return Err(NativeIndexError::Invalid("receipt digest canonicality"));
    }
    Ok(Some(record))
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, NativeIndexError> {
    check_path(path, false)?;
    let input = File::open(path)?;
    let size = input.metadata()?.len();
    if size == 0 || size > limit as u64 {
        return Err(NativeIndexError::Invalid("file size"));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    input.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        return Err(NativeIndexError::Invalid("file changed during read"));
    }
    Ok(bytes)
}

fn unix_now() -> Result<u64, NativeIndexError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| NativeIndexError::Invalid("system clock"))?
        .as_secs())
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::super::{
        native_approval::{NativePublisherKey, signing_message},
        native_trust::NativePolicyPin,
    };
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    struct Fixture {
        root: PathBuf,
        approvals: PathBuf,
        trust: NativeReleaseTrust,
        state: NativeReleaseIndexStore,
        key: Ed25519KeyPair,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("native-index-{}", Uuid::new_v4()));
            let approvals = root.join("approvals");
            let state_root = root.join("index-state");
            let config = root.join("config");
            for dir in [&root, &approvals, &state_root, &config] {
                fs::create_dir_all(dir).unwrap();
            }
            let key = Ed25519KeyPair::from_pkcs8(
                Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .unwrap()
                    .as_ref(),
            )
            .unwrap();
            let policy = serde_json::json!({
                "schema_version": 1,
                "publishers": [{
                    "publisher_id": "native-author",
                    "ed25519_public_key_base64": STANDARD.encode(key.public_key().as_ref()),
                }]
            });
            let policy_bytes = serde_json::to_vec(&policy).unwrap();
            let policy_file = config.join("native-publisher-keys.json");
            fs::write(&policy_file, &policy_bytes).unwrap();
            let sha = hex_digest(digest(&SHA256, &policy_bytes).as_ref().try_into().unwrap());
            let pin = NativePolicyPin::from_sha256_hex(&sha).unwrap();
            Self {
                root,
                approvals: approvals.clone(),
                trust: NativeReleaseTrust::new(policy_file, approvals, Some(pin)),
                state: NativeReleaseIndexStore::new(state_root),
                key,
            }
        }

        fn release(&self, model: &str, manifest_sha256: &str) -> SignedNativeRelease {
            SignedNativeRelease {
                schema_version: 1,
                publisher_id: "native-author".into(),
                model_id: model.into(),
                manifest_sha256: manifest_sha256.into(),
                signature_base64: STANDARD.encode(
                    self.key
                        .sign(signing_message("native-author", model, manifest_sha256).as_bytes())
                        .as_ref(),
                ),
            }
        }

        fn index(&self, generation: u64, releases: Vec<SignedNativeRelease>) -> Vec<u8> {
            let mut signed = SignedNativeReleaseIndex {
                schema_version: 1,
                publisher_id: "native-author".into(),
                generation,
                expires_unix_secs: unix_now().unwrap() + 24 * 60 * 60,
                releases,
                signature_base64: String::new(),
            };
            signed.signature_base64 = STANDARD.encode(
                self.key
                    .sign(index_signing_message(&signed).as_bytes())
                    .as_ref(),
            );
            serde_json::to_vec(&signed).unwrap()
        }

        fn verify(&self, bytes: &[u8]) -> Result<VerifiedNativeReleaseIndex, NativeIndexError> {
            verify_index(
                bytes,
                &self.trust.load_verifier().unwrap(),
                "native-author",
                unix_now().unwrap(),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn publisher_index_provisions_only_independently_verified_approvals() {
        let fixture = Fixture::new();
        let bytes = fixture.index(
            1,
            vec![
                fixture.release("voice", &"12".repeat(32)),
                fixture.release("voice", &"34".repeat(32)),
            ],
        );
        let index = fixture.verify(&bytes).unwrap();
        assert_eq!(index.release_count(), 2);
        let result = fixture
            .state
            .accept(
                &fixture.trust,
                &bytes,
                "native-author",
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(result.generation, 1);
        assert_eq!(result.approvals_written, 2);
        assert!(!result.replayed);
        assert_eq!(fixture.trust.available_releases().unwrap().len(), 2);
        let second = fixture
            .state
            .accept(
                &fixture.trust,
                &bytes,
                "native-author",
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(second.approvals_written, 0);
        assert!(second.replayed);
    }

    #[test]
    fn index_monotonicity_prevents_rollback_and_same_generation_equivocation() {
        let fixture = Fixture::new();
        let first = fixture.index(2, vec![fixture.release("voice", &"11".repeat(32))]);
        fixture
            .state
            .accept(
                &fixture.trust,
                &first,
                "native-author",
                &CancellationToken::new(),
            )
            .unwrap();
        let previous = fixture.index(1, vec![fixture.release("voice", &"22".repeat(32))]);
        assert!(matches!(
            fixture.state.accept(
                &fixture.trust,
                &previous,
                "native-author",
                &CancellationToken::new()
            ),
            Err(NativeIndexError::Rollback)
        ));
        let same_sequence_changed =
            fixture.index(2, vec![fixture.release("voice", &"33".repeat(32))]);
        assert!(matches!(
            fixture.state.accept(
                &fixture.trust,
                &same_sequence_changed,
                "native-author",
                &CancellationToken::new()
            ),
            Err(NativeIndexError::Rollback)
        ));
        assert_eq!(fixture.trust.available_releases().unwrap().len(), 1);
    }

    #[test]
    fn a_forged_or_cross_publisher_index_cannot_enroll_a_key() {
        let fixture = Fixture::new();
        let original = fixture.index(1, vec![fixture.release("voice", &"11".repeat(32))]);
        let mut altered: SignedNativeReleaseIndex = serde_json::from_slice(&original).unwrap();
        altered.generation = 2;
        assert!(
            fixture
                .verify(&serde_json::to_vec(&altered).unwrap())
                .is_err()
        );
        altered = serde_json::from_slice(&original).unwrap();
        altered.releases[0].manifest_sha256 = "22".repeat(32);
        assert!(
            fixture
                .verify(&serde_json::to_vec(&altered).unwrap())
                .is_err()
        );
        assert!(
            verify_index(
                &original,
                &fixture.trust.load_verifier().unwrap(),
                "different-author",
                unix_now().unwrap()
            )
            .is_err()
        );
        let attacker = Ed25519KeyPair::from_pkcs8(
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        altered = serde_json::from_slice(&original).unwrap();
        altered.signature_base64 = STANDARD.encode(
            attacker
                .sign(index_signing_message(&altered).as_bytes())
                .as_ref(),
        );
        assert!(
            fixture
                .verify(&serde_json::to_vec(&altered).unwrap())
                .is_err()
        );
    }

    #[test]
    fn index_expiry_duplicates_and_bounds_fail_closed() {
        let fixture = Fixture::new();
        let original = fixture.index(1, vec![fixture.release("voice", &"11".repeat(32))]);
        let mut altered: SignedNativeReleaseIndex = serde_json::from_slice(&original).unwrap();
        altered.expires_unix_secs = unix_now().unwrap().saturating_sub(1);
        assert!(matches!(
            fixture.verify(&serde_json::to_vec(&altered).unwrap()),
            Err(NativeIndexError::Expired)
        ));
        altered.expires_unix_secs = unix_now().unwrap() + MAX_EXPIRY_HORIZON + 10;
        assert!(
            fixture
                .verify(&serde_json::to_vec(&altered).unwrap())
                .is_err()
        );
        altered = serde_json::from_slice(&original).unwrap();
        altered.releases.push(altered.releases[0].clone());
        assert!(
            fixture
                .verify(&serde_json::to_vec(&altered).unwrap())
                .is_err()
        );
        assert!(fixture.verify(&vec![b'X'; MAX_INDEX_BYTES + 1]).is_err());
        assert!(fixture.verify(b"{}").is_err());
    }

    #[test]
    fn canceled_or_unpinned_index_never_publishes_release() {
        let fixture = Fixture::new();
        let bytes = fixture.index(1, vec![fixture.release("voice", &"11".repeat(32))]);
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            fixture
                .state
                .accept(&fixture.trust, &bytes, "native-author", &token),
            Err(NativeIndexError::Canceled)
        ));
        assert!(fixture.trust.available_releases().unwrap().is_empty());
        let unpinned = NativeReleaseTrust::new(
            fixture.root.join("config/native-publisher-keys.json"),
            fixture.approvals.clone(),
            None,
        );
        assert!(matches!(
            fixture.state.accept(
                &unpinned,
                &bytes,
                "native-author",
                &CancellationToken::new()
            ),
            Err(NativeIndexError::Trust(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_or_approval_root_is_not_followed() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let bytes = fixture.index(1, vec![fixture.release("voice", &"11".repeat(32))]);
        let replacement = fixture.root.join("link");
        symlink(&fixture.approvals, &replacement).unwrap();
        let untrusted = NativeReleaseTrust::new(
            fixture.root.join("config/native-publisher-keys.json"),
            replacement,
            Some(
                NativePolicyPin::from_sha256_hex(&hex_digest(
                    digest(
                        &SHA256,
                        &fs::read(fixture.root.join("config/native-publisher-keys.json")).unwrap(),
                    )
                    .as_ref()
                    .try_into()
                    .unwrap(),
                ))
                .unwrap(),
            ),
        );
        assert!(
            fixture
                .state
                .accept(
                    &untrusted,
                    &bytes,
                    "native-author",
                    &CancellationToken::new()
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn network_redirect_is_rejected_without_importing_approvals() {
        let fixture = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handler = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 2048];
            let _ = socket.read(&mut bytes).await;
            socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/evil\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        });
        let origin = Url::parse(&format!("http://{address}/published/")).unwrap();
        let result = NativeReleaseIndexTransfer::new(Duration::from_secs(5))
            .unwrap()
            .refresh_from_url(
                &origin,
                "native-author",
                fixture.trust.clone(),
                fixture.state.clone(),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(NativeIndexError::HttpStatus)));
        handler.await.unwrap();
        assert!(fixture.trust.available_releases().unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_streamed_index_refresh_verifies_and_publishes_signed_approvals() {
        let fixture = Fixture::new();
        let payload = fixture.index(4, vec![fixture.release("voice", &"ab".repeat(32))]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handler = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let read = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("GET /release/indexes/native-author/index.json HTTP/1.1"));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&payload).await.unwrap();
        });
        let origin = Url::parse(&format!("http://{address}/release/")).unwrap();
        let accepted = NativeReleaseIndexTransfer::new(Duration::from_secs(5))
            .unwrap()
            .refresh_from_url(
                &origin,
                "native-author",
                fixture.trust.clone(),
                fixture.state.clone(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.generation, 4);
        assert_eq!(accepted.approvals_written, 1);
        assert_eq!(fixture.trust.available_releases().unwrap().len(), 1);
        handler.await.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_at_approval_target_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let digest_hex = "ab".repeat(32);
        let bytes = fixture.index(1, vec![fixture.release("voice", &digest_hex)]);
        let model_dir = fixture.approvals.join("voice");
        fs::create_dir(&model_dir).unwrap();
        symlink("/dev/null", model_dir.join(format!("{digest_hex}.json"))).unwrap();
        assert!(
            fixture
                .state
                .accept(
                    &fixture.trust,
                    &bytes,
                    "native-author",
                    &CancellationToken::new(),
                )
                .is_err()
        );
    }

    #[test]
    fn verifier_never_accepts_an_untrusted_publisher_key_as_data() {
        let fixture = Fixture::new();
        let approved = NativePublisherVerifier::new([NativePublisherKey {
            publisher_id: "native-author".into(),
            ed25519_public_key: [42; 32],
        }])
        .unwrap();
        let bytes = fixture.index(1, vec![fixture.release("voice", &"11".repeat(32))]);
        assert!(verify_index(&bytes, &approved, "native-author", unix_now().unwrap()).is_err());
    }
}
