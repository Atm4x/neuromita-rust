use std::{fs, path::PathBuf, process::Command, time::Duration};

use neuromita::ai_runtime::native_model::{
    NativeBackend, NativeFailure, NativeModelError, NativeModelLaunch, NativeModelProcess,
    NativePhase,
};
use neuromita::presentation::native_model::NativeModelPresenter;
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    launch: NativeModelLaunch,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("neuromita-native-model-{}", Uuid::new_v4()));
        let model_directory = root.join("weights");
        let runtime_directory = root.join("runtime");
        fs::create_dir_all(&model_directory).unwrap();
        fs::create_dir_all(&runtime_directory).unwrap();
        let executable = root.join(if cfg!(windows) {
            "worker.exe"
        } else {
            "worker"
        });
        let status = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(format!(
                "{}/tests/fixtures/native_protocol_worker.rs",
                env!("CARGO_MANIFEST_DIR")
            ))
            .arg("-o")
            .arg(&executable)
            .status()
            .unwrap();
        assert!(status.success());
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(&fs::read(&executable).unwrap());
        let mut executable_sha256 = [0u8; 32];
        executable_sha256.copy_from_slice(digest.finish().as_ref());
        let launch = NativeModelLaunch {
            model_id: "native-test".into(),
            executable,
            executable_sha256,
            model_directory,
            runtime_directory,
            backend: NativeBackend::Cpu,
            request_timeout: Duration::from_secs(3),
            speech_contract_version: None,
            speech_capabilities: Vec::new(),
        };
        Self { root, launch }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn one_executable_loads_once_and_services_multiple_requests() {
    let fixture = Fixture::new();
    let mut process = NativeModelProcess::start(fixture.launch.clone())
        .await
        .unwrap();
    let initial = process.snapshot();
    assert_eq!(initial.phase, NativePhase::Started);
    assert!(!NativeModelPresenter::project(&initial).can_infer);
    assert_eq!(process.refresh().await.unwrap().phase, NativePhase::Started);
    assert!(matches!(
        process.infer(json!({})).await,
        Err(NativeModelError::Phase)
    ));
    process.load().await.unwrap();
    assert_eq!(process.snapshot().phase, NativePhase::Loaded);
    process.warmup().await.unwrap();
    assert_eq!(process.snapshot().phase, NativePhase::Warm);
    assert!(!NativeModelPresenter::project(&process.snapshot()).can_infer);
    let ready = process.refresh().await.unwrap();
    assert_eq!(ready.phase, NativePhase::Ready);
    assert!(NativeModelPresenter::project(&ready).can_infer);
    let first = process.infer(json!({"input":"first"})).await.unwrap();
    let second = process.infer(json!({"input":"second"})).await.unwrap();
    assert_eq!(first["sequence"], 1);
    assert_eq!(second["sequence"], 2);
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(
        first["pid"].as_u64().unwrap(),
        u64::from(process.process_id().unwrap())
    );
    assert_eq!(process.snapshot().session_id, initial.session_id);
    process.unload().await.unwrap();
    assert_eq!(process.snapshot().phase, NativePhase::Started);
    process.load().await.unwrap();
    process.warmup().await.unwrap();
    process.refresh().await.unwrap();
    let third = process.infer(json!({"input":"third"})).await.unwrap();
    assert_eq!(third["sequence"], 3);
    process.shutdown().await.unwrap();
}

#[tokio::test]
async fn backend_not_reported_by_worker_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.launch.backend = NativeBackend::Cuda;
    assert!(matches!(
        NativeModelProcess::start(fixture.launch.clone()).await,
        Err(NativeModelError::Protocol)
    ));
}

#[tokio::test]
async fn wrong_correlated_response_is_rejected() {
    let fixture = Fixture::new();
    fs::write(fixture.launch.model_directory.join("wrong-id"), "").unwrap();
    assert!(matches!(
        NativeModelProcess::start(fixture.launch.clone()).await,
        Err(NativeModelError::Protocol)
    ));
}

#[tokio::test]
async fn timed_out_infer_invalidates_session_and_never_returns_ready() {
    let mut fixture = Fixture::new();
    fs::write(fixture.launch.model_directory.join("hang-infer"), "").unwrap();
    fixture.launch.request_timeout = Duration::from_millis(80);
    let mut process = NativeModelProcess::start(fixture.launch.clone())
        .await
        .unwrap();
    process.load().await.unwrap();
    process.warmup().await.unwrap();
    process.refresh().await.unwrap();
    assert!(matches!(
        process.infer(json!({})).await,
        Err(NativeModelError::Timeout)
    ));
    assert_eq!(process.snapshot().phase, NativePhase::Failed);
    assert_eq!(
        process.snapshot().last_failure,
        Some(NativeFailure::Timeout)
    );
    assert!(!NativeModelPresenter::project(&process.snapshot()).can_infer);
    assert!(matches!(
        process.infer(json!({})).await,
        Err(NativeModelError::Phase)
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_model_directory_is_rejected_before_launch() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let link = fixture.root.join("weights-link");
    symlink(&fixture.launch.model_directory, &link).unwrap();
    let mut launch = fixture.launch.clone();
    launch.model_directory = link;
    assert!(matches!(
        launch.validate(),
        Err(NativeModelError::Invalid(_))
    ));
}

#[tokio::test]
async fn changed_executable_fails_closed_before_any_worker_is_spawned() {
    let fixture = Fixture::new();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&fixture.launch.executable)
        .unwrap()
        .write_all(b"tamper")
        .unwrap();
    assert!(matches!(
        NativeModelProcess::start(fixture.launch.clone()).await,
        Err(NativeModelError::Integrity)
    ));
}

#[tokio::test]
async fn worker_crash_during_infer_revokes_ready_phase() {
    let fixture = Fixture::new();
    fs::write(fixture.launch.model_directory.join("crash-infer"), "").unwrap();
    let mut process = NativeModelProcess::start(fixture.launch.clone())
        .await
        .unwrap();
    process.load().await.unwrap();
    process.warmup().await.unwrap();
    process.refresh().await.unwrap();
    assert!(matches!(
        process.infer(json!({})).await,
        Err(NativeModelError::Exited)
    ));
    assert_eq!(process.snapshot().phase, NativePhase::Failed);
    assert_eq!(
        process.snapshot().last_failure,
        Some(NativeFailure::Process)
    );
}

#[tokio::test]
async fn oversized_ipc_frame_is_rejected_without_unbounded_buffer_growth() {
    let fixture = Fixture::new();
    fs::write(fixture.launch.model_directory.join("oversized-frame"), "").unwrap();
    assert!(matches!(
        NativeModelProcess::start(fixture.launch.clone()).await,
        Err(NativeModelError::Protocol)
    ));
}
