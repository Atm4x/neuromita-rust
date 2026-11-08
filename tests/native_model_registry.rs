use std::{fs, path::PathBuf, process::Command, time::Duration};

use neuromita::ai_runtime::{
    native_model::{NativeBackend, NativeModelProcess, NativePhase},
    native_registry::{
        NativeFileRef, NativeModelManifest, NativeModelRegistry, NativeRegistryError,
        NativeRuntimeFile,
    },
};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    runtime: PathBuf,
    package: PathBuf,
    manifest: NativeModelManifest,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native-registration-{}", Uuid::new_v4()));
        let registry = root.join("packages");
        let package = registry.join("native-test");
        let runtime = root.join("shared-runtime");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("models")).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let executable = package.join(if cfg!(windows) {
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
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::write(
            package.join("models/weights.bin"),
            b"test-only-no-rvc-weights",
        )
        .unwrap();
        fs::write(runtime.join("runtime.bin"), b"test-only-runtime").unwrap();
        let manifest = NativeModelManifest {
            schema_version: 1,
            model_id: "native-test".into(),
            display_name: "Test-only Rust native process".into(),
            package_version: "1.0.0".into(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            protocol_version: 1,
            backends: vec![NativeBackend::Cpu],
            executable: file_ref(
                &executable,
                if cfg!(windows) {
                    "bin/worker.exe"
                } else {
                    "bin/worker"
                },
            ),
            assets: vec![file_ref(
                &package.join("models/weights.bin"),
                "models/weights.bin",
            )],
            runtime_files: vec![NativeRuntimeFile {
                backends: vec![NativeBackend::Cpu],
                file: file_ref(&runtime.join("runtime.bin"), "runtime.bin"),
            }],
        };
        let fixture = Self {
            root,
            runtime,
            package,
            manifest,
        };
        fixture.write_manifest();
        fixture
    }

    fn write_manifest(&self) {
        fs::write(
            self.package.join("native-model.json"),
            serde_json::to_vec_pretty(&self.manifest).unwrap(),
        )
        .unwrap();
    }

    fn discover(
        &self,
    ) -> Result<
        Vec<neuromita::ai_runtime::native_registry::NativeModelRegistration>,
        NativeRegistryError,
    > {
        NativeModelRegistry::discover(&self.root.join("packages"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file_ref(path: &std::path::Path, relative_path: &str) -> NativeFileRef {
    let content = fs::read(path).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, &content);
    NativeFileRef {
        path: relative_path.into(),
        size_bytes: content.len() as u64,
        sha256: digest.as_ref().iter().map(|b| format!("{b:02x}")).collect(),
    }
}

#[tokio::test]
async fn discovered_package_launches_real_rust_process_and_reuses_inference_session() {
    let fixture = Fixture::new();
    let packages = fixture.discover().unwrap();
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].manifest().model_id, "native-test");
    let launch = packages[0]
        .prepare_launch(
            packages[0].manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3),
        )
        .unwrap();
    let mut process = NativeModelProcess::start(launch).await.unwrap();
    process.load().await.unwrap();
    process.warmup().await.unwrap();
    assert_eq!(process.refresh().await.unwrap().phase, NativePhase::Ready);
    let a = process.infer(json!({"input":"first"})).await.unwrap();
    let b = process.infer(json!({"input":"second"})).await.unwrap();
    assert_eq!(a["sequence"], 1);
    assert_eq!(b["sequence"], 2);
    assert_eq!(a["pid"], b["pid"]);
    process.shutdown().await.unwrap();
}

#[test]
fn mutating_weights_after_discovery_prevents_launch() {
    let fixture = Fixture::new();
    let registration = fixture.discover().unwrap().remove(0);
    fs::write(
        fixture.package.join("models/weights.bin"),
        b"mutated-not-rvc-weights",
    )
    .unwrap();
    assert!(matches!(
        registration.prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::Integrity)
    ));
}

#[test]
fn mutating_shared_runtime_after_discovery_prevents_launch() {
    let fixture = Fixture::new();
    let registration = fixture.discover().unwrap().remove(0);
    fs::write(fixture.runtime.join("runtime.bin"), b"mutated-runtime!!").unwrap();
    assert!(matches!(
        registration.prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::Integrity)
    ));
}

#[test]
fn changed_manifest_cannot_silently_repin_artifacts() {
    let fixture = Fixture::new();
    let registration = fixture.discover().unwrap().remove(0);
    fs::write(fixture.package.join("native-model.json"), b"{}").unwrap();
    assert!(matches!(
        registration.prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::ChangedManifest)
    ));
}

#[test]
fn unauthorized_cuda_backend_never_admitted() {
    let fixture = Fixture::new();
    let registration = fixture.discover().unwrap().remove(0);
    assert!(matches!(
        registration.prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cuda,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::Incompatible)
    ));
}

#[test]
fn traversal_and_duplicate_manifest_paths_are_rejected() {
    let mut fixture = Fixture::new();
    fixture.manifest.assets[0].path = "../escape.bin".into();
    fixture.write_manifest();
    assert!(matches!(
        fixture.discover(),
        Err(NativeRegistryError::Invalid(_))
    ));
    fixture.manifest.assets[0].path = fixture.manifest.executable.path.clone();
    fixture.write_manifest();
    assert!(matches!(
        fixture.discover(),
        Err(NativeRegistryError::Invalid(_))
    ));
}

#[test]
fn model_id_and_actual_directory_must_agree() {
    let mut fixture = Fixture::new();
    fixture.manifest.model_id = "different-model".into();
    fixture.write_manifest();
    assert!(matches!(
        fixture.discover(),
        Err(NativeRegistryError::Invalid(_))
    ));
}

#[test]
fn malformed_manifest_and_unrecognized_schema_fail_closed() {
    let mut fixture = Fixture::new();
    fs::write(
        fixture.package.join("native-model.json"),
        b"{ \"unexpected\": 1 }",
    )
    .unwrap();
    assert!(matches!(
        fixture.discover(),
        Err(NativeRegistryError::Json(_))
    ));
    fixture.manifest.schema_version = 42;
    fixture.write_manifest();
    assert!(matches!(
        fixture.discover(),
        Err(NativeRegistryError::Invalid(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_asset_and_registry_entry_are_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let registration = fixture.discover().unwrap().remove(0);
    fs::rename(
        fixture.package.join("models/weights.bin"),
        fixture.root.join("actual-weight"),
    )
    .unwrap();
    symlink(
        fixture.root.join("actual-weight"),
        fixture.package.join("models/weights.bin"),
    )
    .unwrap();
    assert!(matches!(
        registration.prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::Invalid(_))
    ));
    symlink(
        &fixture.package,
        fixture.root.join("packages").join("another"),
    )
    .unwrap();
    assert!(matches!(
        fixture.discover(),
        Err(NativeRegistryError::Invalid(_))
    ));
}

#[test]
fn native_manifest_is_not_worker_readiness() {
    let fixture = Fixture::new();
    let packages = fixture.discover().unwrap();
    assert_eq!(packages.len(), 1);
    let prepared = packages[0]
        .prepare_launch(
            packages[0].manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3),
        )
        .unwrap();
    assert_eq!(prepared.model_id, "native-test");
}

#[test]
fn unapproved_manifest_digest_is_never_executable() {
    let fixture = Fixture::new();
    let registration = fixture.discover().unwrap().remove(0);
    let mut wrong_digest = registration.manifest_sha256();
    wrong_digest[0] ^= 0xff;
    assert!(matches!(
        registration.prepare_launch(
            wrong_digest,
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::Unapproved)
    ));
}

#[test]
fn cpu_does_not_require_files_only_needed_by_cuda_backend() {
    let mut fixture = Fixture::new();
    fixture.manifest.backends.push(NativeBackend::Cuda);
    let extra = fixture.runtime.join("cuda-only.dll");
    fs::write(&extra, b"stub-cuda-dependency-not-actually-loaded").unwrap();
    fixture.manifest.runtime_files.push(NativeRuntimeFile {
        backends: vec![NativeBackend::Cuda],
        file: file_ref(&extra, "cuda-only.dll"),
    });
    fixture.write_manifest();
    let registration = fixture.discover().unwrap().remove(0);
    fs::remove_file(&extra).unwrap();
    registration
        .prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cpu,
            &fixture.runtime,
            Duration::from_secs(3),
        )
        .unwrap();
    assert!(matches!(
        registration.prepare_launch(
            registration.manifest_sha256(),
            NativeBackend::Cuda,
            &fixture.runtime,
            Duration::from_secs(3)
        ),
        Err(NativeRegistryError::Io(_))
    ));
}
