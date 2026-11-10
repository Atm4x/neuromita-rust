use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use neuromita::ai_runtime::{
    native_controller::{NativeModelController, NativeModelSelection},
    native_model::NativeBackend,
    native_package_install::{NativePackageInstallError, NativePackageInstaller},
    native_registry::{NativeFileRef, NativeModelManifest, NativeRuntimeFile},
};
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    source_packages: PathBuf,
    source_runtime: PathBuf,
    packages: PathBuf,
    runtime: PathBuf,
    manifest: NativeModelManifest,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("native-local-install-{}", Uuid::new_v4()));
        let source_packages = root.join("source-packages");
        let source_runtime = root.join("source-runtime");
        let packages = root.join("packages");
        let runtime = root.join("runtime");
        for directory in [
            source_packages.join("native-test/bin"),
            source_packages.join("native-test/models"),
            source_runtime.join("shared"),
            packages.clone(),
            runtime.clone(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let executable = source_packages.join(if cfg!(windows) {
            "native-test/bin/worker.exe"
        } else {
            "native-test/bin/worker"
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
            source_packages.join("native-test/models/weight.bin"),
            b"test-only-not-a-neural-model",
        )
        .unwrap();
        fs::write(
            source_runtime.join("shared/native-runtime.bin"),
            b"test-only-native-runtime",
        )
        .unwrap();
        let manifest = NativeModelManifest {
            schema_version: 1,
            model_id: "native-test".into(),
            display_name: "Test-only model process".into(),
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
                &source_packages.join("native-test/models/weight.bin"),
                "models/weight.bin",
            )],
            runtime_files: vec![NativeRuntimeFile {
                backends: vec![NativeBackend::Cpu],
                file: file_ref(
                    &source_runtime.join("shared/native-runtime.bin"),
                    "shared/native-runtime.bin",
                ),
            }],
        };
        let fixture = Self {
            root,
            source_packages,
            source_runtime,
            packages,
            runtime,
            manifest,
        };
        fixture.write_manifest();
        fixture
    }

    fn write_manifest(&self) {
        fs::write(
            self.source_packages.join("native-test/native-model.json"),
            serde_json::to_vec_pretty(&self.manifest).unwrap(),
        )
        .unwrap();
    }

    fn approved(&self) -> [u8; 32] {
        let bytes = fs::read(self.source_packages.join("native-test/native-model.json")).unwrap();
        let hash = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let mut digest = [0; 32];
        digest.copy_from_slice(hash.as_ref());
        digest
    }

    fn installer(&self) -> NativePackageInstaller {
        NativePackageInstaller::new(self.packages.clone(), self.runtime.clone())
    }

    fn install(
        &self,
        approved: [u8; 32],
        backend: NativeBackend,
    ) -> Result<
        neuromita::ai_runtime::native_package_install::NativePackageInstallResult,
        NativePackageInstallError,
    > {
        self.installer().install_local(
            &self.source_packages,
            &self.source_runtime,
            "native-test",
            approved,
            backend,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file_ref(path: &Path, relative: &str) -> NativeFileRef {
    let content = fs::read(path).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, &content);
    NativeFileRef {
        path: relative.into(),
        size_bytes: content.len() as u64,
        sha256: digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

#[tokio::test]
async fn installed_model_activates_through_existing_controller_and_reuses_pid() {
    let fixture = Fixture::new();
    let digest = fixture.approved();
    let installed = fixture.install(digest, NativeBackend::Cpu).unwrap();
    assert!(!installed.reused_package);
    assert_eq!(installed.new_runtime_files, 1);
    assert_eq!(installed.manifest_sha256, digest);
    let mut controller =
        NativeModelController::new(fixture.packages.clone(), fixture.runtime.clone());
    let state = controller
        .activate(&NativeModelSelection {
            model_id: "native-test".into(),
            approved_manifest_sha256: digest,
            backend: NativeBackend::Cpu,
            request_timeout: Duration::from_secs(3),
        })
        .await
        .unwrap();
    assert_eq!(
        state.phase,
        neuromita::ai_runtime::native_model::NativePhase::Ready
    );
    let first = controller.infer(json!({"input":"one"})).await.unwrap();
    let second = controller.infer(json!({"input":"two"})).await.unwrap();
    assert_eq!(first["sequence"], 1);
    assert_eq!(second["sequence"], 2);
    assert_eq!(first["pid"], second["pid"]);
    controller.deactivate().await.unwrap();
}

#[test]
fn repeated_install_reuses_unchanged_package_and_runtime() {
    let fixture = Fixture::new();
    let digest = fixture.approved();
    fixture.install(digest, NativeBackend::Cpu).unwrap();
    let result = fixture.install(digest, NativeBackend::Cpu).unwrap();
    assert!(result.reused_package);
    assert_eq!(result.new_runtime_files, 0);
}

#[test]
fn compatible_runtime_blob_is_shared_between_two_pinned_model_packages() {
    let fixture = Fixture::new();
    fixture
        .install(fixture.approved(), NativeBackend::Cpu)
        .unwrap();
    let second = fixture.source_packages.join("native-other");
    fs::create_dir_all(second.join("bin")).unwrap();
    fs::create_dir_all(second.join("models")).unwrap();
    fs::copy(
        fixture.source_packages.join(format!(
            "native-test/bin/{}",
            if cfg!(windows) {
                "worker.exe"
            } else {
                "worker"
            }
        )),
        second.join(format!(
            "bin/{}",
            if cfg!(windows) {
                "worker.exe"
            } else {
                "worker"
            }
        )),
    )
    .unwrap();
    fs::copy(
        fixture
            .source_packages
            .join("native-test/models/weight.bin"),
        second.join("models/weight.bin"),
    )
    .unwrap();
    let mut manifest = fixture.manifest.clone();
    manifest.model_id = "native-other".into();
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(second.join("native-model.json"), &bytes).unwrap();
    let mut digest = [0; 32];
    digest.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
    let installed = fixture
        .installer()
        .install_local(
            &fixture.source_packages,
            &fixture.source_runtime,
            "native-other",
            digest,
            NativeBackend::Cpu,
        )
        .unwrap();
    assert_eq!(installed.new_runtime_files, 0);
    assert!(!installed.reused_package);
    assert!(
        fixture
            .packages
            .join("native-test/native-model.json")
            .is_file()
    );
    assert!(
        fixture
            .packages
            .join("native-other/native-model.json")
            .is_file()
    );
}

#[test]
fn unapproved_manifest_never_publishes_package_or_runtime() {
    let fixture = Fixture::new();
    let result = fixture.install([0; 32], NativeBackend::Cpu);
    assert!(matches!(
        result,
        Err(NativePackageInstallError::Registry(_))
    ));
    assert!(!fixture.packages.join("native-test").exists());
    assert!(!fixture.runtime.join("shared/native-runtime.bin").exists());
}

#[test]
fn mutated_source_weight_is_rejected_without_publication() {
    let fixture = Fixture::new();
    let digest = fixture.approved();
    fs::write(
        fixture
            .source_packages
            .join("native-test/models/weight.bin"),
        b"modified",
    )
    .unwrap();
    assert!(fixture.install(digest, NativeBackend::Cpu).is_err());
    assert!(!fixture.packages.join("native-test").exists());
    assert!(!fixture.runtime.join("shared/native-runtime.bin").exists());
}

#[test]
fn conflicting_shared_runtime_is_never_replaced() {
    let fixture = Fixture::new();
    let digest = fixture.approved();
    fs::create_dir_all(fixture.runtime.join("shared")).unwrap();
    fs::write(
        fixture.runtime.join("shared/native-runtime.bin"),
        b"other-runtime",
    )
    .unwrap();
    assert!(matches!(
        fixture.install(digest, NativeBackend::Cpu),
        Err(NativePackageInstallError::RuntimeConflict)
    ));
    assert_eq!(
        fs::read(fixture.runtime.join("shared/native-runtime.bin")).unwrap(),
        b"other-runtime"
    );
    assert!(!fixture.packages.join("native-test").exists());
}

#[test]
fn second_revision_is_denied_without_changing_published_model() {
    let mut fixture = Fixture::new();
    let original = fixture.approved();
    fixture.install(original, NativeBackend::Cpu).unwrap();
    fixture.manifest.package_version = "2.0.0".into();
    fixture.write_manifest();
    let second = fixture.approved();
    assert!(matches!(
        fixture.install(second, NativeBackend::Cpu),
        Err(NativePackageInstallError::RevisionConflict)
    ));
    assert_eq!(
        fs::read(fixture.packages.join("native-test/native-model.json")).unwrap(),
        serde_json::to_vec_pretty(&NativeModelManifest {
            package_version: "1.0.0".into(),
            ..fixture.manifest.clone()
        })
        .unwrap()
    );
}

#[test]
fn cpu_does_not_download_cuda_only_native_components() {
    let mut fixture = Fixture::new();
    fixture.manifest.backends.push(NativeBackend::Cuda);
    fixture.manifest.runtime_files.push(NativeRuntimeFile {
        backends: vec![NativeBackend::Cuda],
        file: NativeFileRef {
            path: "shared/cublas-test.bin".into(),
            size_bytes: 9999,
            sha256: "0".repeat(64),
        },
    });
    fixture.write_manifest();
    fixture
        .install(fixture.approved(), NativeBackend::Cpu)
        .unwrap();
    assert!(!fixture.runtime.join("shared/cublas-test.bin").exists());
    assert!(fixture.runtime.join("shared/native-runtime.bin").is_file());
}

#[test]
fn recovery_removes_only_uuid_orphans_and_preserves_foreign_staging() {
    let fixture = Fixture::new();
    let parent = fixture.root.join(".packages.native-staging");
    fs::create_dir_all(parent.join(Uuid::new_v4().to_string())).unwrap();
    fs::write(parent.join("foreign-not-ours"), b"preserve").unwrap();
    let orphan = fs::read_dir(&parent)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    fixture
        .install(fixture.approved(), NativeBackend::Cpu)
        .unwrap();
    assert!(!orphan.exists());
    assert_eq!(
        fs::read(parent.join("foreign-not-ours")).unwrap(),
        b"preserve"
    );
}

#[test]
fn busy_owner_lock_prevents_reclaim_of_another_active_install() {
    let fixture = Fixture::new();
    let stage = fixture.root.join(".packages.native-staging");
    fs::create_dir_all(&stage).unwrap();
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(stage.join(".install-owner.lock"))
        .unwrap();
    owner.lock().unwrap();
    let live = stage.join(Uuid::new_v4().to_string());
    fs::create_dir(&live).unwrap();
    assert!(matches!(
        fixture.install(fixture.approved(), NativeBackend::Cpu),
        Err(NativePackageInstallError::Busy)
    ));
    assert!(live.is_dir());
    drop(owner);
    fixture
        .install(fixture.approved(), NativeBackend::Cpu)
        .unwrap();
    assert!(!live.exists());
}

#[test]
fn overlapping_source_and_managed_roots_are_rejected_before_install() {
    let fixture = Fixture::new();
    let result = fixture.installer().install_local(
        &fixture.packages,
        &fixture.source_runtime,
        "native-test",
        fixture.approved(),
        NativeBackend::Cpu,
    );
    assert!(matches!(result, Err(NativePackageInstallError::Invalid(_))));
    assert!(!fixture.packages.join("native-test").exists());
}

#[cfg(unix)]
#[test]
fn foreign_symlinked_staging_directory_fails_closed_without_erasure() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let staging = fixture.root.join(".packages.native-staging");
    fs::create_dir_all(&staging).unwrap();
    let victim = fixture.root.join("do-not-remove");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("data"), b"preserved").unwrap();
    symlink(&victim, staging.join(Uuid::new_v4().to_string())).unwrap();
    assert!(matches!(
        fixture.install(fixture.approved(), NativeBackend::Cpu),
        Err(NativePackageInstallError::Invalid(_))
    ));
    assert_eq!(fs::read(victim.join("data")).unwrap(), b"preserved");
    assert!(!fixture.packages.join("native-test").exists());
}

#[cfg(unix)]
#[test]
fn dangling_runtime_symlink_is_not_treated_as_missing() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.runtime.join("shared")).unwrap();
    symlink(
        "no-such-file",
        fixture.runtime.join("shared/native-runtime.bin"),
    )
    .unwrap();
    assert!(matches!(
        fixture.install(fixture.approved(), NativeBackend::Cpu),
        Err(NativePackageInstallError::RuntimeConflict)
    ));
    assert!(!fixture.packages.join("native-test").exists());
}

#[test]
fn interrupted_owner_fixture() {
    let Ok(path) = std::env::var("NM_NATIVE_TEST_CRASH_STAGE") else {
        return;
    };
    let stage = PathBuf::from(path);
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(stage.join(".install-owner.lock"))
        .unwrap();
    owner.lock().unwrap();
    fs::create_dir(stage.join(Uuid::new_v4().to_string())).unwrap();
    fs::write(stage.join("ready"), b"yes").unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn os_kill_reclaims_abandoned_stage_without_erasing_published_revision() {
    let fixture = Fixture::new();
    fixture
        .install(fixture.approved(), NativeBackend::Cpu)
        .unwrap();
    let parent = fixture.root.join(".packages.native-staging");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interrupted_owner_fixture", "--nocapture"])
        .env("NM_NATIVE_TEST_CRASH_STAGE", &parent)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !parent.join("ready").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(parent.join("ready").exists());
    assert!(matches!(
        fixture.install(fixture.approved(), NativeBackend::Cpu),
        Err(NativePackageInstallError::Busy)
    ));
    child.kill().unwrap();
    child.wait().unwrap();
    let published = fixture.packages.join("native-test/native-model.json");
    let before = fs::read(&published).unwrap();
    fixture
        .install(fixture.approved(), NativeBackend::Cpu)
        .unwrap();
    assert_eq!(fs::read(published).unwrap(), before);
    assert_eq!(
        fs::read_dir(&parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().unwrap().is_dir())
            .count(),
        0
    );
}
