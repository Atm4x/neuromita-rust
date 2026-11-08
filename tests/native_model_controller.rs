use std::{fs, path::PathBuf, process::Command, time::Duration};

use neuromita::ai_runtime::{
    native_controller::{NativeControllerError, NativeModelController, NativeModelSelection},
    native_model::{NativeBackend, NativeFailure, NativeModelError, NativePhase},
    native_registry::{NativeFileRef, NativeModelManifest, NativeRegistryError},
};
use neuromita::presentation::native_model::NativeModelPresenter;
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    manifest_sha: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native-controller-{}", Uuid::new_v4()));
        let package = root.join("packages/native-test");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("models")).unwrap();
        fs::create_dir_all(root.join("runtime")).unwrap();
        let worker = package.join(if cfg!(windows) {
            "bin/worker.exe"
        } else {
            "bin/worker"
        });
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(format!(
                "{}/tests/fixtures/native_protocol_worker.rs",
                env!("CARGO_MANIFEST_DIR")
            ))
            .arg("-o")
            .arg(&worker)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let weights = package.join("models/weights.bin");
        fs::write(&weights, b"synthetic-fixture-not-RVC-weights").unwrap();
        let manifest = NativeModelManifest {
            schema_version: 1,
            model_id: "native-test".into(),
            display_name: "Test model".into(),
            package_version: "1.0.0".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            protocol_version: 1,
            backends: vec![NativeBackend::Cpu],
            executable: file_ref(
                &worker,
                if cfg!(windows) {
                    "bin/worker.exe"
                } else {
                    "bin/worker"
                },
            ),
            assets: vec![file_ref(&weights, "models/weights.bin")],
            runtime_files: vec![],
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(package.join("native-model.json"), &manifest_bytes).unwrap();
        Self {
            root,
            manifest_sha: sha256(&manifest_bytes),
        }
    }

    fn controller(&self) -> NativeModelController {
        NativeModelController::new(self.root.join("packages"), self.root.join("runtime"))
    }

    fn selection(&self) -> NativeModelSelection {
        NativeModelSelection {
            model_id: "native-test".into(),
            approved_manifest_sha256: self.manifest_sha,
            backend: NativeBackend::Cpu,
            request_timeout: Duration::from_secs(3),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut value = [0; 32];
    value.copy_from_slice(digest.as_ref());
    value
}

fn file_ref(path: &std::path::Path, name: &str) -> NativeFileRef {
    let bytes = fs::read(path).unwrap();
    NativeFileRef {
        path: name.into(),
        size_bytes: bytes.len() as u64,
        sha256: sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect(),
    }
}

#[tokio::test]
async fn controller_activates_one_process_reuses_it_and_deactivates() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    assert!(controller.status().await.unwrap().is_none());
    assert_eq!(controller.discover().unwrap().len(), 1);
    let started = controller.activate(&fixture.selection()).await.unwrap();
    assert_eq!(started.phase, NativePhase::Ready);
    let status = controller.status().await.unwrap().unwrap();
    assert_eq!(status.session_id, started.session_id);
    assert!(NativeModelPresenter::project(&status).can_infer);
    let first = controller.infer(json!({"input":"first"})).await.unwrap();
    let second = controller.infer(json!({"input":"second"})).await.unwrap();
    assert_eq!(first["sequence"], 1);
    assert_eq!(second["sequence"], 2);
    assert_eq!(first["pid"], second["pid"]);
    controller.deactivate().await.unwrap();
    assert!(controller.status().await.unwrap().is_none());
    assert!(matches!(
        controller.infer(json!({})).await,
        Err(NativeControllerError::NotActive)
    ));
}

#[tokio::test]
async fn selection_requires_external_approval_and_never_auto_approves_discovery() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let mut choice = fixture.selection();
    choice.approved_manifest_sha256 = [0; 32];
    assert!(matches!(
        controller.activate(&choice).await,
        Err(NativeControllerError::Registry(
            NativeRegistryError::Unapproved
        ))
    ));
    assert!(controller.status().await.unwrap().is_none());
    controller.activate(&fixture.selection()).await.unwrap();
    controller.deactivate().await.unwrap();
}

#[tokio::test]
async fn active_session_cannot_be_replaced_without_explicit_shutdown() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let id = controller
        .activate(&fixture.selection())
        .await
        .unwrap()
        .session_id;
    assert!(matches!(
        controller.activate(&fixture.selection()).await,
        Err(NativeControllerError::AlreadyActive)
    ));
    assert_eq!(controller.status().await.unwrap().unwrap().session_id, id);
    assert_eq!(controller.infer(json!({})).await.unwrap()["sequence"], 1);
    controller.deactivate().await.unwrap();
    let next_id = controller
        .activate(&fixture.selection())
        .await
        .unwrap()
        .session_id;
    assert_ne!(id, next_id);
    assert_eq!(controller.infer(json!({})).await.unwrap()["sequence"], 1);
    controller.deactivate().await.unwrap();
}

#[tokio::test]
async fn package_changes_and_unsupported_backends_are_rejected() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let mut selection = fixture.selection();
    selection.backend = NativeBackend::Cuda;
    assert!(matches!(
        controller.activate(&selection).await,
        Err(NativeControllerError::Registry(
            NativeRegistryError::Incompatible
        ))
    ));
    fs::write(
        fixture.root.join("packages/native-test/models/weights.bin"),
        b"wrong",
    )
    .unwrap();
    assert!(matches!(
        controller.activate(&fixture.selection()).await,
        Err(NativeControllerError::Registry(
            NativeRegistryError::Integrity
        ))
    ));
    assert!(controller.status().await.unwrap().is_none());
}

#[tokio::test]
async fn unknown_model_is_not_started_or_silently_replaced() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    let mut selection = fixture.selection();
    selection.model_id = "not-present".into();
    assert!(matches!(
        controller.activate(&selection).await,
        Err(NativeControllerError::NotFound)
    ));
    assert!(controller.status().await.unwrap().is_none());
}

#[tokio::test]
async fn crashed_inference_revokes_readiness_until_explicit_reactivation() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("packages/native-test/crash-infer"), b"").unwrap();
    let mut controller = fixture.controller();
    controller.activate(&fixture.selection()).await.unwrap();
    assert!(matches!(
        controller.infer(json!({})).await,
        Err(NativeControllerError::Process(NativeModelError::Exited))
    ));
    let failed = controller.status().await.unwrap().unwrap();
    assert_eq!(failed.phase, NativePhase::Failed);
    assert_eq!(failed.last_failure, Some(NativeFailure::Process));
    assert!(!NativeModelPresenter::project(&failed).can_infer);
    assert!(matches!(
        controller.activate(&fixture.selection()).await,
        Err(NativeControllerError::AlreadyActive)
    ));
    controller.deactivate().await.unwrap();
    assert!(controller.status().await.unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn actual_external_worker_death_is_detected_on_status_poll() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller();
    controller.activate(&fixture.selection()).await.unwrap();
    let pid = controller.infer(json!({})).await.unwrap()["pid"]
        .as_u64()
        .unwrap();
    assert!(
        Command::new("kill")
            .arg("-9")
            .arg(pid.to_string())
            .status()
            .unwrap()
            .success()
    );
    assert!(matches!(
        controller.status().await,
        Err(NativeControllerError::Process(NativeModelError::Exited))
    ));
    let failed = controller.last_observed_snapshot().unwrap();
    assert_eq!(failed.phase, NativePhase::Failed);
    assert!(!NativeModelPresenter::project(&failed).can_infer);
    controller.deactivate().await.unwrap();
}
