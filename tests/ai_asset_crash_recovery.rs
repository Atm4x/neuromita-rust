#![cfg(unix)]

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use neuromita::ai_runtime::{
    assets::{AssetInput, AssetSource, ModelAssetError, ModelAssetStore},
    install::{ModelAssetRequirement, PluginInstallPlan},
    install_operation::{InstallOperation, OperationStage, StageState},
    overlay::{LockedOverlay, LockedPackage, OverlayInstaller},
};
use ring::digest::{Context, SHA256};
use uuid::Uuid;

const SIZE: u64 = 256 * 1024 * 1024;

fn store(root: &Path) -> ModelAssetStore {
    ModelAssetStore {
        root: root.join("AIData/models"),
        max_asset_bytes: SIZE,
        timeout: Duration::from_secs(40),
    }
}

fn selection(root: &Path, hash: &str) -> (PluginInstallPlan, Vec<AssetInput>) {
    let plan = PluginInstallPlan {
        build_stages: vec![],
        asset_hooks: vec![],
        packages: vec![],
        requirements: vec![],
        backend: None,
        supported_backends: vec![],
        required_backends: vec![],
        supported_python_minors: vec![],
        actions: vec![],
        assets: vec![ModelAssetRequirement {
            role: "weight".into(),
            relative_path: "weights.bin".into(),
            required: true,
            sha256: Some(hash.to_owned()),
        }],
    };
    let input = AssetInput {
        role: "weight".into(),
        relative_path: "weights.bin".into(),
        sha256: hash.to_owned(),
        source: AssetSource::LocalFile(root.join("source.bin")),
    };
    (plan, vec![input])
}

fn stages() -> Vec<OperationStage> {
    vec![OperationStage {
        id: "asr:assets:weight".into(),
        repeatable: true,
    }]
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn asset_publish_interrupted_child() {
    let Ok(root) = std::env::var("NM_ASSET_INTERRUPTED_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let hash = std::fs::read_to_string(root.join("sha256")).unwrap();
    let data = root.join("AIData");
    let mut operation = InstallOperation::open(&data, &"a".repeat(64), stages()).unwrap();
    operation.start(0).unwrap();
    std::fs::write(
        root.join("operation-id"),
        operation.operation_id().to_string(),
    )
    .unwrap();
    let (plan, inputs) = selection(&root, &hash);
    store(&root)
        .publish("mock_asr", "v1", &plan, &inputs)
        .await
        .unwrap();
    panic!("asset transfer must be interrupted before publication");
}

#[tokio::test]
async fn killed_asset_transfer_recovers_only_owned_orphans_and_verifies_receipt() {
    let root = std::env::temp_dir().join(format!("nm-asset-kill-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join("AIData/models/mock_asr")).unwrap();
    let source = File::create(root.join("source.bin")).unwrap();
    source.set_len(SIZE).unwrap();
    drop(source);
    let mut hasher = Context::new(&SHA256);
    let zeros = [0_u8; 65536];
    for _ in 0..SIZE / zeros.len() as u64 {
        hasher.update(&zeros);
    }
    let hash: String = hasher
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::fs::write(root.join("sha256"), &hash).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "asset_publish_interrupted_child", "--nocapture"])
        .env("NM_ASSET_INTERRUPTED_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let parent = root.join("AIData/models/mock_asr");
    let deadline = Instant::now() + Duration::from_secs(25);
    let staging = loop {
        assert!(
            Instant::now() < deadline,
            "asset copy did not reach staging"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before interruption"
        );
        if let Ok(entries) = std::fs::read_dir(&parent) {
            if let Some(path) = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(".staging-"))
                        && path
                            .join("weights.bin")
                            .metadata()
                            .is_ok_and(|meta| meta.len() > 0 && meta.len() < SIZE)
                })
            {
                break path;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    };
    let lock = PathBuf::from(format!("{}.lock", staging.display()));
    assert!(lock.is_file());
    let id = std::fs::read_to_string(root.join("operation-id")).unwrap();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(!parent.join("v1").exists(), "partial asset cannot publish");

    let live = parent.join(format!(".staging-{}", Uuid::new_v4()));
    std::fs::create_dir(&live).unwrap();
    let live_lock = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(format!("{}.lock", live.display()))
        .unwrap();
    live_lock.lock().unwrap();
    let unowned = parent.join(format!(".staging-{}", Uuid::new_v4()));
    std::fs::create_dir(&unowned).unwrap();
    std::fs::write(unowned.join("preserve"), b"not owned").unwrap();

    let mut operation =
        InstallOperation::open(&root.join("AIData"), &"a".repeat(64), stages()).unwrap();
    assert_eq!(operation.operation_id().to_string(), id);
    assert_eq!(operation.stage_state(0).unwrap(), StageState::Started);
    assert!(operation.finish().is_err());
    let (plan, inputs) = selection(&root, &hash);
    let store = store(&root);
    let ready = store
        .publish("mock_asr", "v1", &plan, &inputs)
        .await
        .unwrap();
    assert!(store.verified_revision("mock_asr", "v1").await.is_ok());
    assert!(
        !staging.exists(),
        "abandoned partial asset must be reclaimed"
    );
    assert!(!lock.exists(), "orphan ownership marker must be removed");
    assert!(live.is_dir(), "active stage must not be reclaimed");
    assert!(
        unowned.join("preserve").is_file(),
        "unowned stage must not be reclaimed"
    );
    operation.verified(0).unwrap();
    operation.finish().unwrap();

    std::fs::write(ready.join("weights.bin"), b"corrupted").unwrap();
    assert!(matches!(
        store.verified_revision("mock_asr", "v1").await,
        Err(ModelAssetError::Conflict)
    ));
    assert!(matches!(
        store.publish("mock_asr", "v1", &plan, &inputs).await,
        Err(ModelAssetError::Conflict)
    ));
    assert!(
        ready.is_dir(),
        "published shared revision must not be deleted"
    );
    assert!(matches!(
        std::fs::read_dir(&live).map(|mut entries| entries.next().is_none()),
        Ok(true)
    ));
    drop(live_lock);
    let (empty_plan, _) = selection(&root, &hash);
    let empty_plan = PluginInstallPlan {
        assets: vec![],
        ..empty_plan
    };
    let empty = store
        .publish("mock_asr", "v2", &empty_plan, &[])
        .await
        .unwrap();
    assert!(empty.is_dir());
    assert!(
        !live.exists(),
        "released active stage must be reclaimed on next publish"
    );
    assert!(unowned.join("preserve").is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
fn uv_installer(root: &Path) -> OverlayInstaller {
    OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(45),
    }
}

#[cfg(target_os = "linux")]
fn overlay_spec(revision: &str, with_package: bool) -> LockedOverlay {
    LockedOverlay {
        plugin_id: "mock_asr".into(),
        revision: revision.into(),
        python_version: "3.12.10".into(),
        packages: if with_package {
            vec![LockedPackage {
                name: "mockpkg".into(),
                version: "1.0.0".into(),
            }]
        } else {
            vec![]
        },
        actions: vec![],
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn uv_install_interrupted_parent_child() {
    let Ok(root) = std::env::var("NM_UV_INTERRUPTED_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut operation = InstallOperation::open(
        &root.join("AIData"),
        &"c".repeat(64),
        vec![OperationStage {
            id: "asr:overlay:mock_asr".into(),
            repeatable: true,
        }],
    )
    .unwrap();
    operation.start(0).unwrap();
    std::fs::write(
        root.join("uv-operation-id"),
        operation.operation_id().to_string(),
    )
    .unwrap();
    let installer = uv_installer(&root);
    installer
        .install(&overlay_spec("pending", true))
        .await
        .unwrap();
    panic!("uv must be interrupted before layer publication");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn killed_uv_parent_preserves_live_child_stage_then_reclaims_and_reinstalls() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("nm-uv-kill-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join("overlays/mock_asr")).unwrap();
    std::fs::create_dir_all(root.join("AIData")).unwrap();
    std::fs::write(root.join("python"), b"fake Python").unwrap();
    let uv = root.join("uv");
    std::fs::write(
        &uv,
        r#"#!/usr/bin/env python3
import os, pathlib, sys, time
args = sys.argv
stage = pathlib.Path(args[args.index('--target') + 1])
root = stage.parent.parent.parent
if not (root / 'uv-release').exists():
    (root / 'uv-ready').write_text(f'{os.getpid()}\n{stage}')
    while not (root / 'uv-release').exists():
        time.sleep(.02)
info = stage / 'mockpkg-1.0.0.dist-info'
info.mkdir(parents=True, exist_ok=True)
(info / 'METADATA').write_text('Name: mockpkg\nVersion: 1.0.0\n')
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&uv).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&uv, permissions).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "uv_install_interrupted_parent_child",
            "--nocapture",
        ])
        .env("NM_UV_INTERRUPTED_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let ready = root.join("uv-ready");
    let deadline = Instant::now() + Duration::from_secs(12);
    while !ready.is_file() {
        assert!(
            Instant::now() < deadline,
            "uv subprocess never entered its install step"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "installer parent terminated early"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let raw = std::fs::read_to_string(&ready).unwrap();
    let (pid, stage) = raw.split_once('\n').unwrap();
    let stage = PathBuf::from(stage);
    let child_pid: u32 = pid.parse().unwrap();
    let lock = PathBuf::from(format!("{}.lock", stage.display()));
    assert!(lock.is_file());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let operation_id = std::fs::read_to_string(root.join("uv-operation-id")).unwrap();
    let mut operation = InstallOperation::open(
        &root.join("AIData"),
        &"c".repeat(64),
        vec![OperationStage {
            id: "asr:overlay:mock_asr".into(),
            repeatable: true,
        }],
    )
    .unwrap();
    assert_eq!(operation.operation_id().to_string(), operation_id);
    assert_eq!(operation.stage_state(0).unwrap(), StageState::Started);
    assert!(operation.finish().is_err());
    assert!(
        PathBuf::from(format!("/proc/{child_pid}")).exists(),
        "uv subprocess must survive its killed parent"
    );

    let installer = uv_installer(&root);
    let stable = root.join("overlays/mock_asr/stable");
    installer
        .install(&overlay_spec("stable", false))
        .await
        .unwrap();
    assert!(stage.is_dir(), "a live uv child must protect its staging");

    std::fs::write(root.join("uv-release"), b"continue").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let owner = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock)
                .unwrap();
            if owner.try_lock().is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("uv child never released staging ownership");

    let pending = root.join("overlays/mock_asr/pending");
    installer
        .install(&overlay_spec("pending", true))
        .await
        .unwrap();
    assert!(
        !stage.exists(),
        "the previously interrupted uv stage must be reclaimed"
    );
    assert!(!lock.exists());
    assert!(
        stable.is_dir(),
        "another published revision must remain intact"
    );
    assert!(pending.join("mockpkg-1.0.0.dist-info/METADATA").is_file());
    assert!(
        installer
            .installed(&overlay_spec("pending", true))
            .await
            .unwrap(),
        "only the published receipt and distribution validate the recovered layer"
    );
    operation.verified(0).unwrap();
    operation.finish().unwrap();
    std::fs::write(pending.join(".neuromita-overlay.json"), b"corrupted").unwrap();
    assert!(
        installer
            .installed(&overlay_spec("pending", true))
            .await
            .is_err()
    );
    assert!(
        installer
            .install(&overlay_spec("pending", true))
            .await
            .is_err()
    );
    assert!(
        stable.is_dir(),
        "corrupt receipt must not delete another revision"
    );
    drop(operation);
    std::fs::remove_dir_all(root).unwrap();
}
