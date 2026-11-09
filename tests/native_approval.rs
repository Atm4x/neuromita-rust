use std::{fs, path::PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use neuromita::ai_runtime::{
    native_approval::{
        NativeApprovalError, NativePublisherKey, NativePublisherVerifier, SignedNativeRelease,
        signing_message,
    },
    native_model::NativeBackend,
    native_registry::{NativeFileRef, NativeModelManifest},
    native_revisions::NativeRevisionStore,
};
use ring::{
    digest::{SHA256, digest},
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use uuid::Uuid;

fn signing_pair() -> Ed25519KeyPair {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

fn verifier(key: &Ed25519KeyPair) -> NativePublisherVerifier {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(key.public_key().as_ref());
    NativePublisherVerifier::new([NativePublisherKey {
        publisher_id: "release-author".into(),
        ed25519_public_key: bytes,
    }])
    .unwrap()
}

fn signed(key: &Ed25519KeyPair, model: &str, digest: &str) -> Vec<u8> {
    let message = signing_message("release-author", model, digest);
    serde_json::to_vec(&SignedNativeRelease {
        schema_version: 1,
        publisher_id: "release-author".into(),
        model_id: model.into(),
        manifest_sha256: digest.into(),
        signature_base64: STANDARD.encode(key.sign(message.as_bytes()).as_ref()),
    })
    .unwrap()
}

fn sha_hex(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn pinned_ed25519_signature_authorizes_exact_manifest_and_model() {
    let key = signing_pair();
    let sha = "a3".repeat(32);
    let release = verifier(&key).verify(&signed(&key, "voice", &sha)).unwrap();
    assert_eq!(release.publisher_id(), "release-author");
    assert_eq!(release.model_id(), "voice");
    assert_eq!(release.manifest_sha256(), [0xa3; 32]);
    assert_eq!(
        signing_message("release-author", "voice", &sha),
        format!("neuromita.native-release.v1\nrelease-author\nvoice\n{sha}\n")
    );
}

#[test]
fn local_digest_without_trusted_publisher_never_proves_approval() {
    let key = signing_pair();
    let unsigned = signed(&key, "voice", &"12".repeat(32));
    assert!(matches!(
        NativePublisherVerifier::new([]).unwrap().verify(&unsigned),
        Err(NativeApprovalError::UnknownPublisher)
    ));
}

#[test]
fn altering_model_or_digest_invalidates_signature() {
    let key = signing_pair();
    let v = verifier(&key);
    let bytes = signed(&key, "voice", &"12".repeat(32));
    for (field, replacement) in [
        ("model_id", "evil".into()),
        ("manifest_sha256", "34".repeat(32)),
        ("publisher_id", "other-author".into()),
    ] {
        let mut doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        doc[field] = serde_json::Value::String(replacement);
        assert!(v.verify(&serde_json::to_vec(&doc).unwrap()).is_err());
    }
}

#[test]
fn another_signing_key_cannot_impersonate_the_pinned_publisher() {
    let authorized = signing_pair();
    let attacker = signing_pair();
    let forged = signed(&attacker, "voice", &"12".repeat(32));
    assert!(matches!(
        verifier(&authorized).verify(&forged),
        Err(NativeApprovalError::InvalidSignature)
    ));
}

#[test]
fn noncanonical_or_oversized_signed_document_fails_closed() {
    let key = signing_pair();
    let v = verifier(&key);
    let bytes = signed(&key, "voice", &"12".repeat(32));
    for (field, replacement) in [
        ("schema_version", serde_json::json!(2)),
        ("manifest_sha256", serde_json::json!("AB".repeat(32))),
        ("signature_base64", serde_json::json!("invalid!")),
    ] {
        let mut doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        doc[field] = replacement;
        assert!(v.verify(&serde_json::to_vec(&doc).unwrap()).is_err());
    }
    assert!(v.verify(&vec![b'X'; 4097]).is_err());
    assert!(v.verify(b"").is_err());
}

#[test]
fn extra_fields_and_duplicate_keys_cannot_override_policy() {
    let key = signing_pair();
    let v = verifier(&key);
    let bytes = signed(&key, "voice", &"12".repeat(32));
    let mut doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    doc["ed25519_public_key"] = serde_json::json!(STANDARD.encode(key.public_key().as_ref()));
    assert!(v.verify(&serde_json::to_vec(&doc).unwrap()).is_err());

    let trusted = NativePublisherKey {
        publisher_id: "release-author".into(),
        ed25519_public_key: [1; 32],
    };
    assert!(matches!(
        NativePublisherVerifier::new([trusted.clone(), trusted]),
        Err(NativeApprovalError::InvalidPolicy(_))
    ));
}

struct LocalFixture {
    root: PathBuf,
    source_packages: PathBuf,
    source_runtime: PathBuf,
    revisions: PathBuf,
    runtime: PathBuf,
    manifest_digest: [u8; 32],
}

impl LocalFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native-trust-{}", Uuid::new_v4()));
        let source_packages = root.join("incoming");
        let source_runtime = root.join("incoming-runtime");
        let revisions = root.join("revisions");
        let runtime = root.join("shared-runtime");
        let package = source_packages.join("test-model");
        let model_files = package.join("models");
        for path in [
            &package,
            &model_files,
            &source_runtime,
            &revisions,
            &runtime,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let executable = package.join("worker");
        fs::write(&executable, b"native-worker-placeholder").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(model_files.join("weight.bin"), b"synthetic-weight").unwrap();
        let manifest = NativeModelManifest {
            schema_version: 1,
            model_id: "test-model".into(),
            display_name: "Signed synthetic package".into(),
            package_version: "1.0".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            protocol_version: 1,
            backends: vec![NativeBackend::Cpu],
            executable: NativeFileRef {
                path: "worker".into(),
                size_bytes: fs::metadata(&executable).unwrap().len(),
                sha256: sha_hex(b"native-worker-placeholder"),
            },
            assets: vec![NativeFileRef {
                path: "models/weight.bin".into(),
                size_bytes: 16,
                sha256: sha_hex(b"synthetic-weight"),
            }],
            runtime_files: vec![],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let digest = digest(&SHA256, &bytes);
        let mut manifest_digest = [0; 32];
        manifest_digest.copy_from_slice(digest.as_ref());
        fs::write(package.join("native-model.json"), bytes).unwrap();
        Self {
            root,
            source_packages,
            source_runtime,
            revisions,
            runtime,
            manifest_digest,
        }
    }

    fn store(&self) -> NativeRevisionStore {
        NativeRevisionStore::new(self.revisions.clone(), self.runtime.clone())
    }
}

impl Drop for LocalFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn externally_verified_release_can_be_installed_without_self_approval() {
    let fixture = LocalFixture::new();
    let key = signing_pair();
    let digest = fixture
        .manifest_digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let approved = verifier(&key)
        .verify(&signed(&key, "test-model", &digest))
        .unwrap();
    let result = fixture
        .store()
        .install_verified_local(
            &fixture.source_packages,
            &fixture.source_runtime,
            &approved,
            NativeBackend::Cpu,
        )
        .unwrap();
    assert_eq!(result.model_id, "test-model");
    assert_eq!(result.manifest_sha256, fixture.manifest_digest);
    assert_eq!(fixture.store().list("test-model").unwrap().len(), 1);
    let workflow =
        neuromita::ai_runtime::native_workflow::NativeModelWorkflow::new(fixture.store());
    workflow
        .select_verified(&approved, NativeBackend::Cpu)
        .unwrap();
    let selected = fixture.store().selected("test-model").unwrap().unwrap();
    assert_eq!(selected.manifest_sha256, fixture.manifest_digest);
    assert!(workflow.snapshot().unwrap().worker.is_none());
}

#[test]
fn signed_but_wrong_digest_cannot_publish_unrelated_local_bytes() {
    let fixture = LocalFixture::new();
    let key = signing_pair();
    let authorized = verifier(&key)
        .verify(&signed(&key, "test-model", &"f0".repeat(32)))
        .unwrap();
    assert!(
        fixture
            .store()
            .install_verified_local(
                &fixture.source_packages,
                &fixture.source_runtime,
                &authorized,
                NativeBackend::Cpu,
            )
            .is_err()
    );
    assert!(fixture.store().list("test-model").unwrap().is_empty());
}
