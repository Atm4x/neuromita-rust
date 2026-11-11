use std::{fs, path::PathBuf, process::Command, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use neuromita::ai_runtime::{
    native_model::{NativeBackend, NativeModelError, NativeModelLaunch, NativeModelProcess},
    native_registry::{NativeFileRef, NativeModelManifest, NativeModelRegistry},
    native_revisions::{NativeRevisionChoice, NativeRevisionStore},
    native_speech::{self, NativeSpeechCapability, NativeSpeechKind},
    native_workflow::NativeModelWorkflow,
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    launch: NativeModelLaunch,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native-speech-ipc-{}", Uuid::new_v4()));
        let model_directory = root.join("models");
        let runtime_directory = root.join("runtime");
        fs::create_dir_all(&model_directory).unwrap();
        fs::create_dir_all(&runtime_directory).unwrap();
        let executable = root.join(if cfg!(windows) {
            "speech.exe"
        } else {
            "speech"
        });
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("--edition=2021")
            .arg(format!(
                "{}/tests/fixtures/native_speech_protocol_worker.rs",
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
        let digest = ring::digest::digest(&ring::digest::SHA256, &fs::read(&executable).unwrap());
        let mut executable_sha256 = [0u8; 32];
        executable_sha256.copy_from_slice(digest.as_ref());
        let launch = NativeModelLaunch {
            model_id: "native-test".into(),
            executable,
            executable_sha256,
            model_directory,
            runtime_directory,
            backend: NativeBackend::Cpu,
            request_timeout: Duration::from_secs(3),
            speech_contract_version: Some(native_speech::CONTRACT_VERSION),
            speech_capabilities: vec![
                NativeSpeechCapability {
                    kind: NativeSpeechKind::Tts,
                    sample_rate_hz: 16_000,
                    languages: vec!["ru".into()],
                },
                NativeSpeechCapability {
                    kind: NativeSpeechKind::Asr,
                    sample_rate_hz: 16_000,
                    languages: vec!["ru".into()],
                },
            ],
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
async fn native_tts_and_asr_use_typed_correlated_ipc_without_python() {
    let fixture = Fixture::new();
    let mut process = NativeModelProcess::start(fixture.launch.clone())
        .await
        .unwrap();
    process.load().await.unwrap();
    process.warmup().await.unwrap();
    process.refresh().await.unwrap();
    let caps = process.speech_capabilities().to_vec();
    let tts = native_speech::encode_tts(&caps, "Привет", "speaker", "ru", 80).unwrap();
    let audio = native_speech::decode_tts(&caps, "ru", process.infer(tts).await.unwrap()).unwrap();
    assert_eq!(audio.bytes.len(), 46);
    let wav = STANDARD
        .decode("UklGRiYAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQIAAAAAAA==")
        .unwrap();
    let asr = native_speech::encode_asr(&caps, &wav, "ru").unwrap();
    let transcript = native_speech::decode_asr(process.infer(asr).await.unwrap()).unwrap();
    assert_eq!(transcript.text, "hello");
    process.shutdown().await.unwrap();
}

#[tokio::test]
async fn worker_cannot_claim_unapproved_speech_capabilities() {
    let fixture = Fixture::new();
    let mut launch = fixture.launch.clone();
    launch.speech_capabilities.pop();
    assert!(matches!(
        NativeModelProcess::start(launch).await,
        Err(NativeModelError::Protocol)
    ));
}

#[tokio::test]
async fn legacy_generic_model_cannot_advertise_speech() {
    let fixture = Fixture::new();
    let mut launch = fixture.launch.clone();
    launch.speech_capabilities.clear();
    launch.speech_contract_version = None;
    assert!(matches!(
        NativeModelProcess::start(launch).await,
        Err(NativeModelError::Protocol)
    ));
}

#[tokio::test]
async fn manifest_v2_attested_worker_can_infer_but_pinned_digest_must_match() {
    let fixture = Fixture::new();
    let root = fixture.root.join("registry");
    let package = root.join("native-test");
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::create_dir_all(package.join("assets")).unwrap();
    let relative_executable = if cfg!(windows) {
        "bin/speech.exe"
    } else {
        "bin/speech"
    };
    let worker = package.join(relative_executable);
    fs::copy(&fixture.launch.executable, &worker).unwrap();
    let weights = package.join("assets/mock.bin");
    fs::write(&weights, b"synthetic manifest test weights").unwrap();
    let file_ref = |path: &std::path::Path, relative: &str| {
        let bytes = fs::read(path).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
        NativeFileRef {
            path: relative.into(),
            size_bytes: bytes.len() as u64,
            sha256: digest.as_ref().iter().map(|v| format!("{v:02x}")).collect(),
        }
    };
    let manifest = NativeModelManifest {
        schema_version: 2,
        model_id: "native-test".into(),
        display_name: "fixture speech worker".into(),
        package_version: "1.0.0".into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        protocol_version: 1,
        backends: vec![NativeBackend::Cpu],
        executable: file_ref(&worker, relative_executable),
        assets: vec![file_ref(&weights, "assets/mock.bin")],
        runtime_files: vec![],
        speech_contract_version: fixture.launch.speech_contract_version,
        speech_capabilities: fixture.launch.speech_capabilities.clone(),
    };
    fs::write(
        package.join("native-model.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let registration = NativeModelRegistry::discover(&root).unwrap().remove(0);
    let approved_digest = registration.manifest_sha256();
    let mut unapproved_digest = approved_digest;
    unapproved_digest[0] ^= 1;
    assert!(
        registration
            .prepare_launch(
                unapproved_digest,
                NativeBackend::Cpu,
                &fixture.launch.runtime_directory,
                Duration::from_secs(3)
            )
            .is_err()
    );
    let launch = registration
        .prepare_launch(
            approved_digest,
            NativeBackend::Cpu,
            &fixture.launch.runtime_directory,
            Duration::from_secs(3),
        )
        .unwrap();
    let mut worker = NativeModelProcess::start(launch).await.unwrap();
    worker.load().await.unwrap();
    worker.warmup().await.unwrap();
    worker.refresh().await.unwrap();
    let caps = worker.speech_capabilities().to_vec();
    let request = native_speech::encode_tts(&caps, "Привет", "default", "ru", 75).unwrap();
    assert!(native_speech::decode_tts(&caps, "ru", worker.infer(request).await.unwrap()).is_ok());
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn distinct_service_processes_do_not_serialize_native_ipc() {
    let fixture = Fixture::new();
    let source_packages = fixture.root.join("source-packages");
    let source_runtime = fixture.root.join("source-runtime");
    let runtime = fixture.root.join("installed-runtime");
    let revisions = fixture.root.join("revisions");
    let package = source_packages.join("native-test");
    fs::create_dir_all(package.join("bin")).unwrap();
    fs::create_dir_all(&source_runtime).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::create_dir_all(&revisions).unwrap();
    let binary = if cfg!(windows) {
        "speech.exe"
    } else {
        "speech"
    };
    let worker = package.join("bin").join(binary);
    fs::copy(&fixture.launch.executable, &worker).unwrap();
    fs::create_dir_all(package.join("assets")).unwrap();
    let weight = package.join("assets/fixture.bin");
    fs::write(&weight, b"two-service synthetic model").unwrap();
    let file = |path: &std::path::Path, relative: &str| {
        let bytes = fs::read(path).unwrap();
        NativeFileRef {
            path: relative.to_string(),
            size_bytes: bytes.len() as u64,
            sha256: ring::digest::digest(&ring::digest::SHA256, &bytes)
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    };
    let manifest = NativeModelManifest {
        schema_version: 2,
        model_id: "native-test".into(),
        display_name: "dual service worker".into(),
        package_version: "1.0.0".into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        protocol_version: 1,
        backends: vec![NativeBackend::Cpu],
        executable: file(&worker, &format!("bin/{binary}")),
        assets: vec![file(&weight, "assets/fixture.bin")],
        runtime_files: vec![],
        speech_contract_version: Some(native_speech::CONTRACT_VERSION),
        speech_capabilities: fixture.launch.speech_capabilities.clone(),
    };
    fs::write(
        package.join("native-model.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let registration = NativeModelRegistry::discover(&source_packages)
        .unwrap()
        .remove(0);
    let digest = registration.manifest_sha256();
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let store = NativeRevisionStore::new(revisions, runtime.clone());
    store
        .install_local(
            &source_packages,
            &source_runtime,
            "native-test",
            digest,
            NativeBackend::Cpu,
        )
        .unwrap();
    store
        .select(&NativeRevisionChoice {
            model_id: "native-test".into(),
            manifest_sha256: digest,
            backend: NativeBackend::Cpu,
        })
        .unwrap();
    let mut workflow = NativeModelWorkflow::new(store);
    workflow
        .activate_for_service(NativeSpeechKind::Tts, "native-test", Duration::from_secs(3))
        .await
        .unwrap();
    workflow
        .activate_for_service(NativeSpeechKind::Asr, "native-test", Duration::from_secs(3))
        .await
        .unwrap();
    assert_eq!(workflow.snapshot().unwrap().service_workers.len(), 2);
    let mut workflow = Arc::new(workflow);
    let hold = {
        let workflow = Arc::clone(&workflow);
        let digest_hex = digest_hex.clone();
        tokio::spawn(async move {
            workflow
                .synthesize_speech("native-test", &digest_hex, "hold-tts", "voice", "ru", 100)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !runtime.join("tts-inflight").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let wav = STANDARD
        .decode("UklGRiYAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQIAAAAAAA==")
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        workflow.transcribe_speech("native-test", &digest_hex, &wav, "ru"),
    )
    .await;
    fs::write(runtime.join("release-tts"), b"release").unwrap();
    assert_eq!(result.unwrap().unwrap().text, "hello");
    assert!(hold.await.unwrap().is_ok());
    Arc::get_mut(&mut workflow)
        .unwrap()
        .deactivate_all()
        .await
        .unwrap();
}
