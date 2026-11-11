use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use neuromita::ai_runtime::{
    native_model::{NativeBackend, NativePhase},
    native_registry::{NativeFileRef, NativeModelManifest, NativeRuntimeFile},
    native_revisions::{NativeRevisionChoice, NativeRevisionStore, NativeSelectionManifestState},
};
use neuromita::presentation::native_revisions::{
    NativeArtifactVerification, NativeRevisionPresenter, NativeWorkerObservation,
};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    source_packages: PathBuf,
    source_runtime: PathBuf,
    runtime: PathBuf,
    revisions: PathBuf,
    manifest: NativeModelManifest,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native-revisions-{}", Uuid::new_v4()));
        let source_packages = root.join("source-packages");
        let source_runtime = root.join("source-runtime");
        let runtime = root.join("runtime");
        let revisions = root.join("revisions");
        for dir in [
            source_packages.join("native-test/bin"),
            source_packages.join("native-test/models"),
            source_runtime.join("shared"),
            runtime.clone(),
            revisions.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let executable_name = if cfg!(windows) {
            "worker.exe"
        } else {
            "worker"
        };
        let executable = source_packages
            .join("native-test/bin")
            .join(executable_name);
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(format!(
                "{}/tests/fixtures/native_protocol_worker.rs",
                env!("CARGO_MANIFEST_DIR")
            ))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let weight = source_packages.join("native-test/models/weight.bin");
        fs::write(&weight, b"not-rvc-weights").unwrap();
        let runtime_file = source_runtime.join("shared/runtime.bin");
        fs::write(&runtime_file, b"shared-test-runtime").unwrap();
        let manifest = NativeModelManifest {
            schema_version: 1,
            model_id: "native-test".into(),
            display_name: "Synthetic worker only".into(),
            package_version: "1.0.0".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            protocol_version: 1,
            backends: vec![NativeBackend::Cpu],
            executable: file_ref(&executable, &format!("bin/{executable_name}")),
            assets: vec![file_ref(&weight, "models/weight.bin")],
            runtime_files: vec![NativeRuntimeFile {
                backends: vec![NativeBackend::Cpu],
                file: file_ref(&runtime_file, "shared/runtime.bin"),
            }],
            speech_contract_version: None,
            speech_capabilities: Vec::new(),
        };
        let fixture = Self {
            root,
            source_packages,
            source_runtime,
            runtime,
            revisions,
            manifest,
        };
        fixture.write_manifest();
        fixture
    }

    fn store(&self) -> NativeRevisionStore {
        NativeRevisionStore::new(self.revisions.clone(), self.runtime.clone())
    }

    fn write_manifest(&self) {
        fs::write(
            self.source_packages.join("native-test/native-model.json"),
            serde_json::to_vec(&self.manifest).unwrap(),
        )
        .unwrap();
    }

    fn digest(&self) -> [u8; 32] {
        let bytes = fs::read(self.source_packages.join("native-test/native-model.json")).unwrap();
        let mut result = [0; 32];
        result.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
        result
    }

    fn version(&mut self, version: &str) -> [u8; 32] {
        self.manifest.package_version = version.into();
        self.write_manifest();
        self.digest()
    }

    fn install(&self, digest: [u8; 32]) {
        self.store()
            .install_local(
                &self.source_packages,
                &self.source_runtime,
                "native-test",
                digest,
                NativeBackend::Cpu,
            )
            .unwrap();
    }

    fn choice(&self, digest: [u8; 32]) -> NativeRevisionChoice {
        NativeRevisionChoice {
            model_id: "native-test".into(),
            manifest_sha256: digest,
            backend: NativeBackend::Cpu,
        }
    }

    fn object_root(&self, digest: [u8; 32]) -> PathBuf {
        let hex = digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.revisions.join("objects").join(hex)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file_ref(path: &Path, relative: &str) -> NativeFileRef {
    let bytes = fs::read(path).unwrap();
    NativeFileRef {
        path: relative.into(),
        size_bytes: bytes.len() as u64,
        sha256: ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    }
}

#[tokio::test]
async fn upgrade_and_rollback_do_not_redirect_resident_worker() {
    let mut fixture = Fixture::new();
    let v1 = fixture.digest();
    fixture.install(v1);
    fixture.store().select(&fixture.choice(v1)).unwrap();
    let (mut old, old_selection) = fixture
        .store()
        .controller_for_selected("native-test", Duration::from_secs(3))
        .unwrap()
        .unwrap();
    assert_eq!(
        old.activate(&old_selection).await.unwrap().phase,
        NativePhase::Ready
    );
    let first = old.infer(json!({"input":"one"})).await.unwrap();

    let v2 = fixture.version("2.0.0");
    assert_ne!(v1, v2);
    fixture.install(v2);
    fixture.store().select(&fixture.choice(v2)).unwrap();
    let (mut new, new_selection) = fixture
        .store()
        .controller_for_selected("native-test", Duration::from_secs(3))
        .unwrap()
        .unwrap();
    assert_eq!(new_selection.approved_manifest_sha256, v2);
    assert_eq!(
        new.activate(&new_selection).await.unwrap().phase,
        NativePhase::Ready
    );
    let second = old.infer(json!({"input":"two"})).await.unwrap();
    let newer = new.infer(json!({"input":"new"})).await.unwrap();
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(second["sequence"], 2);
    assert_eq!(newer["sequence"], 1);

    fixture.store().select(&fixture.choice(v1)).unwrap();
    assert_eq!(
        fixture
            .store()
            .selected("native-test")
            .unwrap()
            .unwrap()
            .manifest_sha256,
        v1
    );
    assert_eq!(fixture.store().list("native-test").unwrap().len(), 2);
    assert!(
        fixture
            .object_root(v1)
            .join("native-test/native-model.json")
            .is_file()
    );
    assert!(
        fixture
            .object_root(v2)
            .join("native-test/native-model.json")
            .is_file()
    );
    assert_eq!(
        new.infer(json!({"input":"still-new"})).await.unwrap()["sequence"],
        2
    );
    old.deactivate().await.unwrap();
    new.deactivate().await.unwrap();
}

#[test]
fn absent_selection_has_no_implicit_newest_fallback() {
    let mut fixture = Fixture::new();
    assert!(fixture.store().selected("native-test").unwrap().is_none());
    let v1 = fixture.digest();
    fixture.install(v1);
    let v2 = fixture.version("1.0.1");
    fixture.install(v2);
    assert_eq!(fixture.store().list("native-test").unwrap().len(), 2);
    assert!(
        fixture
            .store()
            .controller_for_selected("native-test", Duration::from_secs(3))
            .unwrap()
            .is_none()
    );
    assert!(fixture.store().selected("native-test").unwrap().is_none());
}

#[test]
fn same_revision_is_idempotent_and_shares_verified_runtime() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    let second = fixture
        .store()
        .install_local(
            &fixture.source_packages,
            &fixture.source_runtime,
            "native-test",
            digest,
            NativeBackend::Cpu,
        )
        .unwrap();
    assert!(second.reused_package);
    assert_eq!(second.new_runtime_files, 0);
    assert_eq!(fixture.store().list("native-test").unwrap().len(), 1);
}

#[test]
fn unauthorized_revision_does_not_change_existing_selection() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let mut forged = digest;
    forged[0] ^= 0xFF;
    assert!(fixture.store().select(&fixture.choice(forged)).is_err());
    assert!(
        fixture
            .store()
            .install_local(
                &fixture.source_packages,
                &fixture.source_runtime,
                "native-test",
                forged,
                NativeBackend::Cpu
            )
            .is_err()
    );
    assert_eq!(
        fixture
            .store()
            .selected("native-test")
            .unwrap()
            .unwrap()
            .manifest_sha256,
        digest
    );
}

#[test]
fn corrupt_or_torn_selection_fails_closed_without_rewrite() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let file = fixture.revisions.join("selections/native-test.json");
    fs::write(&file, b"{\"schema_version\":1,").unwrap();
    assert!(fixture.store().selected("native-test").is_err());
    assert_eq!(fs::read(&file).unwrap(), b"{\"schema_version\":1,");
    fixture.store().select(&fixture.choice(digest)).unwrap();
    assert_eq!(
        fixture
            .store()
            .selected("native-test")
            .unwrap()
            .unwrap()
            .manifest_sha256,
        digest
    );
}

#[test]
fn missing_published_weights_reject_selected_but_keep_pointer() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let file = fixture
        .object_root(digest)
        .join("native-test/models/weight.bin");
    fs::write(&file, b"changed-by-untrusted-writer").unwrap();
    assert!(fixture.store().selected("native-test").is_err());
    let pointer = fixture.revisions.join("selections/native-test.json");
    assert!(pointer.is_file());
}

#[test]
fn ui_inventory_is_metadata_only_and_never_claims_verified_or_ready() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    fs::write(
        fixture
            .object_root(digest)
            .join("native-test/models/weight.bin"),
        b"deliberately-corrupted-weights",
    )
    .unwrap();

    let overview = fixture.store().overview().unwrap();
    assert_eq!(overview.revisions.len(), 1);
    assert_eq!(overview.selections.len(), 1);
    assert_eq!(
        overview.selections[0].manifest_state,
        NativeSelectionManifestState::Present
    );
    let view = NativeRevisionPresenter::project(overview);
    assert_eq!(view.models[0].model_id, "native-test");
    assert_eq!(view.models[0].package_version, "1.0.0");
    assert_eq!(view.models[0].display_name, "Synthetic worker only");
    assert_eq!(
        view.models[0].artifact_verification,
        NativeArtifactVerification::NotChecked
    );
    assert_eq!(
        view.selections[0].worker_observation,
        NativeWorkerObservation::NotObserved
    );
    assert!(fixture.store().selected("native-test").is_err());
    assert!(
        fixture
            .store()
            .controller_for_selected("native-test", Duration::from_secs(3))
            .is_err()
    );
}

#[test]
fn ui_inventory_tracks_explicit_switch_but_not_a_running_session() {
    let mut fixture = Fixture::new();
    let first = fixture.digest();
    fixture.install(first);
    let second = fixture.version("2.0.0");
    fixture.install(second);
    let without_selection = NativeRevisionPresenter::read(&fixture.store()).unwrap();
    assert_eq!(without_selection.models.len(), 2);
    assert!(without_selection.selections.is_empty());

    fixture.store().select(&fixture.choice(second)).unwrap();
    let selected = NativeRevisionPresenter::read(&fixture.store()).unwrap();
    assert_eq!(selected.selections.len(), 1);
    assert_eq!(selected.selections[0].backend, NativeBackend::Cpu);
    assert_eq!(
        selected.selections[0].manifest_sha256,
        second
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(
        selected.selections[0].manifest_state,
        NativeSelectionManifestState::Present
    );
    assert!(
        selected
            .models
            .iter()
            .all(|model| { model.worker_observation == NativeWorkerObservation::NotObserved })
    );
}

#[test]
fn missing_published_revision_is_reported_without_implicit_fallback() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    fs::remove_dir_all(fixture.object_root(digest).join("native-test")).unwrap();

    let overview = fixture.store().overview().unwrap();
    assert!(overview.revisions.is_empty());
    assert_eq!(overview.selections.len(), 1);
    assert_eq!(
        overview.selections[0].manifest_state,
        NativeSelectionManifestState::Missing
    );
    assert!(fixture.store().selected("native-test").is_err());
}

#[test]
fn unsupported_selected_backend_is_not_silently_changed_by_presenter() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let pointer = fixture.revisions.join("selections/native-test.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&pointer).unwrap()).unwrap();
    record["backend"] = json!("cuda");
    fs::write(&pointer, serde_json::to_vec(&record).unwrap()).unwrap();

    let overview = fixture.store().overview().unwrap();
    assert_eq!(overview.selections[0].choice.backend, NativeBackend::Cuda);
    assert_eq!(
        overview.selections[0].manifest_state,
        NativeSelectionManifestState::BackendUndeclared
    );
    assert!(fixture.store().selected("native-test").is_err());
}

#[test]
fn malformed_selection_and_foreign_selection_entries_fail_closed() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let directory = fixture.revisions.join("selections");
    let pointer = directory.join("native-test.json");
    fs::write(&pointer, b"{broken").unwrap();
    assert!(NativeRevisionPresenter::read(&fixture.store()).is_err());
    fixture.store().select(&fixture.choice(digest)).unwrap();
    fs::write(directory.join("foreign-file"), b"untouched").unwrap();
    assert!(fixture.store().overview().is_err());
    assert_eq!(
        fs::read(directory.join("foreign-file")).unwrap(),
        b"untouched"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_selection_does_not_leak_victim_contents_into_ui() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let pointer = fixture.revisions.join("selections/native-test.json");
    let victim = fixture.root.join("victim.json");
    fs::write(&victim, fs::read(&pointer).unwrap()).unwrap();
    fs::remove_file(&pointer).unwrap();
    symlink(&victim, &pointer).unwrap();
    assert!(fixture.store().overview().is_err());
    assert!(fixture.store().selected("native-test").is_err());
    assert!(victim.is_file());
}

#[test]
fn abandoned_selection_temps_recovered_without_touching_foreign_files() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let selections = fixture.revisions.join("selections");
    let orphan = selections.join(format!(".select-{}.tmp", Uuid::new_v4()));
    fs::write(&orphan, b"incomplete").unwrap();
    fs::write(selections.join("foreign"), b"preserved").unwrap();
    fixture.store().select(&fixture.choice(digest)).unwrap();
    assert!(!orphan.exists());
    assert_eq!(fs::read(selections.join("foreign")).unwrap(), b"preserved");
}

#[cfg(unix)]
#[test]
fn foreign_symlink_selection_stage_rejected_without_erasing_victim() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let selections = fixture.revisions.join("selections");
    let victim = fixture.root.join("victim.txt");
    fs::write(&victim, b"preserved").unwrap();
    symlink(
        &victim,
        selections.join(format!(".select-{}.tmp", Uuid::new_v4())),
    )
    .unwrap();
    assert!(fixture.store().select(&fixture.choice(digest)).is_err());
    assert_eq!(fs::read(victim).unwrap(), b"preserved");
}

#[test]
fn selected_runtime_mutation_fails_closed() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    fs::write(fixture.runtime.join("shared/runtime.bin"), b"changed").unwrap();
    assert!(fixture.store().selected("native-test").is_err());
}

#[test]
fn cross_process_selection_lock_protects_live_owner() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.revisions.join("selections/.install-owner.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(fixture.store().select(&fixture.choice(digest)).is_err());
    lock.unlock().unwrap();
    fixture.store().select(&fixture.choice(digest)).unwrap();
}

#[test]
fn selection_owner_child_fixture() {
    let Ok(path) = std::env::var("NM_TEST_SELECTION_CRASH") else {
        return;
    };
    let root = PathBuf::from(path);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".install-owner.lock"))
        .unwrap();
    lock.lock().unwrap();
    fs::write(
        root.join(format!(".select-{}.tmp", Uuid::new_v4())),
        b"uncommitted",
    )
    .unwrap();
    fs::write(root.join("child-ready"), b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn killed_selection_owner_preserves_previous_pointer_and_reclaims_temp() {
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    let dir = fixture.revisions.join("selections");
    let old = fs::read(dir.join("native-test.json")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "selection_owner_child_fixture", "--nocapture"])
        .env("NM_TEST_SELECTION_CRASH", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !dir.join("child-ready").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dir.join("child-ready").exists());
    assert!(fixture.store().select(&fixture.choice(digest)).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(fs::read(dir.join("native-test.json")).unwrap(), old);
    fixture.store().select(&fixture.choice(digest)).unwrap();
    assert_eq!(fs::read(dir.join("native-test.json")).unwrap(), old);
    assert!(
        fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".select-"))
    );
}

#[tokio::test]
async fn application_workflow_requires_explicit_selection_before_activation() {
    use neuromita::ai_runtime::native_workflow::{NativeModelWorkflow, NativeWorkflowError};
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    let mut workflow = NativeModelWorkflow::new(fixture.store());
    let snapshot = workflow.snapshot().unwrap();
    assert_eq!(snapshot.inventory.revisions.len(), 1);
    assert!(snapshot.inventory.selections.is_empty());
    assert!(snapshot.worker.is_none());
    assert!(matches!(
        workflow
            .activate_selected("native-test", Duration::from_secs(3))
            .await,
        Err(NativeWorkflowError::NotSelected)
    ));
    workflow
        .select("native-test", &hex_digest(digest), NativeBackend::Cpu)
        .unwrap();
    let snapshot = workflow.snapshot().unwrap();
    assert_eq!(snapshot.inventory.selections.len(), 1);
    assert!(snapshot.worker.is_none());
    assert_eq!(
        workflow
            .activate_selected("native-test", Duration::from_secs(3))
            .await
            .unwrap()
            .phase,
        NativePhase::Ready
    );
    workflow.deactivate().await.unwrap();
}

#[tokio::test]
async fn application_workflow_selection_change_does_not_redirect_active_process() {
    use neuromita::ai_runtime::native_workflow::{NativeModelWorkflow, NativeWorkflowError};
    let mut fixture = Fixture::new();
    let old_digest = fixture.digest();
    fixture.install(old_digest);
    let mut workflow = NativeModelWorkflow::new(fixture.store());
    workflow
        .select("native-test", &hex_digest(old_digest), NativeBackend::Cpu)
        .unwrap();
    let old_snapshot = workflow
        .activate_selected("native-test", Duration::from_secs(3))
        .await
        .unwrap();
    let first = workflow.infer(json!({"input":"old"})).await.unwrap();

    let new_digest = fixture.version("2.0.0");
    fixture.install(new_digest);
    workflow
        .select("native-test", &hex_digest(new_digest), NativeBackend::Cpu)
        .unwrap();
    let view = workflow.snapshot().unwrap();
    assert_eq!(
        view.active_revision.as_ref().unwrap().manifest_sha256,
        old_digest
    );
    assert_eq!(
        view.inventory.selections[0].choice.manifest_sha256,
        new_digest
    );
    let second = workflow.infer(json!({"input":"still old"})).await.unwrap();
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(second["sequence"], 2);
    assert_eq!(
        workflow.refresh_worker().await.unwrap().unwrap().session_id,
        old_snapshot.session_id
    );
    assert!(matches!(
        workflow
            .activate_selected("native-test", Duration::from_secs(3))
            .await,
        Err(NativeWorkflowError::AlreadyActive)
    ));
    workflow.deactivate().await.unwrap();
    let new_snapshot = workflow
        .activate_selected("native-test", Duration::from_secs(3))
        .await
        .unwrap();
    assert_ne!(new_snapshot.session_id, old_snapshot.session_id);
    assert_eq!(
        workflow
            .snapshot()
            .unwrap()
            .active_revision
            .unwrap()
            .manifest_sha256,
        new_digest
    );
    assert_eq!(
        workflow.infer(json!({"input":"new"})).await.unwrap()["sequence"],
        1
    );
    workflow.deactivate().await.unwrap();
}

#[tokio::test]
async fn application_workflow_refuses_unapproved_or_corrupt_content() {
    use neuromita::ai_runtime::native_workflow::NativeModelWorkflow;
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    let mut workflow = NativeModelWorkflow::new(fixture.store());
    assert!(
        workflow
            .select("native-test", &"0".repeat(64), NativeBackend::Cpu)
            .is_err()
    );
    assert!(workflow.snapshot().unwrap().inventory.selections.is_empty());
    workflow
        .select("native-test", &hex_digest(digest), NativeBackend::Cpu)
        .unwrap();
    fs::write(
        fixture
            .object_root(digest)
            .join("native-test/models/weight.bin"),
        b"modified",
    )
    .unwrap();
    let overview = workflow.snapshot().unwrap();
    assert_eq!(overview.inventory.revisions.len(), 1);
    assert!(overview.worker.is_none());
    assert!(
        workflow
            .activate_selected("native-test", Duration::from_secs(3))
            .await
            .is_err()
    );
    assert!(workflow.snapshot().unwrap().worker.is_none());
}

#[tokio::test]
async fn application_workflow_reports_worker_failure_without_changing_selection() {
    use neuromita::ai_runtime::native_workflow::NativeModelWorkflow;
    let fixture = Fixture::new();
    let digest = fixture.digest();
    fixture.install(digest);
    fs::write(
        fixture.object_root(digest).join("native-test/crash-infer"),
        b"",
    )
    .unwrap();
    let mut workflow = NativeModelWorkflow::new(fixture.store());
    workflow
        .select("native-test", &hex_digest(digest), NativeBackend::Cpu)
        .unwrap();
    workflow
        .activate_selected("native-test", Duration::from_secs(3))
        .await
        .unwrap();
    assert!(workflow.infer(json!({})).await.is_err());
    let state = workflow.snapshot().unwrap();
    assert_eq!(state.worker.unwrap().phase, NativePhase::Failed);
    assert_eq!(state.inventory.selections[0].choice.manifest_sha256, digest);
    workflow.deactivate().await.unwrap();
    assert!(workflow.snapshot().unwrap().worker.is_none());
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
