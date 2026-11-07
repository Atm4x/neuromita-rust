//! Filesystem publication and uv execution shared by backend layers and plugin overlays.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use uuid::Uuid;

use super::{
    distribution,
    overlay::{InstallerAction, LockedPackage},
};

#[derive(Debug, Error)]
pub enum LayerError {
    #[error("invalid package layer: {0}")]
    Invalid(String),
    #[error("package layer is not installed: {0}")]
    NotInstalled(String),
    #[error("immutable revision already exists with different contents")]
    Conflict,
    #[error("package layer filesystem failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("uv package operation timed out")]
    Timeout,
    #[error("uv package operation failed: {0}")]
    Install(String),
    #[error("installed package layer is incomplete or corrupted: {0}")]
    Corrupted(String),
}

const STAGING_LOCK_SUFFIX: &str = ".lock";

fn staging_lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(STAGING_LOCK_SUFFIX);
    PathBuf::from(lock)
}

fn remove_abandoned_staging(path: &Path) -> bool {
    match std::fs::remove_dir_all(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to clean abandoned Python layer staging");
            false
        }
    }
}

fn remove_stage_lock(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to remove Python layer staging lock");
    }
}

#[cfg(not(windows))]
fn is_owned_stage_name(name: &str) -> bool {
    name.strip_prefix(".staging-")
        .and_then(|id| {
            Uuid::parse_str(id)
                .ok()
                .filter(|uuid| uuid.to_string() == id)
        })
        .is_some()
}

fn open_stage_owner(lock_path: &Path) -> Option<File> {
    if !std::fs::symlink_metadata(lock_path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0)
    {
        return None;
    }
    let owner = match OpenOptions::new().read(true).write(true).open(lock_path) {
        Ok(owner) => owner,
        Err(error) => {
            tracing::warn!(path = %lock_path.display(), error = %error, "cannot open staging ownership lock");
            return None;
        }
    };
    match owner.try_lock() {
        Ok(()) => Some(owner),
        Err(std::fs::TryLockError::WouldBlock) => None,
        Err(error) => {
            tracing::warn!(path = %lock_path.display(), error = %error, "cannot lock abandoned staging");
            None
        }
    }
}

#[cfg(windows)]
fn reclaim_abandoned_staging(_parent: &Path) -> Result<(), LayerError> {
    Ok(())
}

#[cfg(not(windows))]
fn reclaim_abandoned_staging(parent: &Path) -> Result<(), LayerError> {
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_owned_stage_name(name) || !entry.file_type()?.is_dir() {
            continue;
        }
        let stage_path = entry.path();
        let lock_path = staging_lock_path(&stage_path);
        let Some(owner) = open_stage_owner(&lock_path) else {
            continue;
        };
        match std::fs::symlink_metadata(&stage_path) {
            Ok(metadata) if metadata.is_dir() => {
                if !remove_abandoned_staging(&stage_path) {
                    continue;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => continue,
        }
        remove_stage_lock(&lock_path);
        drop(owner);
    }
    Ok(())
}

struct StagingCleanup {
    path: PathBuf,
    lock_path: PathBuf,
    owner: Option<File>,
    published: bool,
    child_may_be_running: bool,
}

impl StagingCleanup {
    fn new(path: PathBuf) -> Result<Self, LayerError> {
        let lock_path = staging_lock_path(&path);
        let owner = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if let Err(error) = owner.lock() {
            remove_stage_lock(&lock_path);
            return Err(LayerError::Io(error));
        }
        if let Err(error) = std::fs::create_dir(&path) {
            remove_stage_lock(&lock_path);
            return Err(LayerError::Io(error));
        }
        Ok(Self {
            path,
            lock_path,
            owner: Some(owner),
            published: false,
            child_may_be_running: false,
        })
    }

    fn child_stdin(&mut self) -> Result<Stdio, LayerError> {
        let owner = self
            .owner
            .as_ref()
            .ok_or_else(|| LayerError::Invalid("staging owner unavailable".into()))?;
        let child = owner.try_clone()?;
        self.child_may_be_running = true;
        Ok(Stdio::from(child))
    }

    fn child_finished(&mut self) {
        self.child_may_be_running = false;
    }

    fn disarm(&mut self) {
        self.published = true;
        remove_stage_lock(&self.lock_path);
    }
}

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        #[cfg(windows)]
        if self.child_may_be_running {
            tracing::warn!(path = %self.path.display(), "staging retained: Windows cannot prove a surviving uv child released ownership");
            return;
        }
        let path = std::mem::take(&mut self.path);
        let lock_path = std::mem::take(&mut self.lock_path);
        let owner = self.owner.take();
        let cleanup = move || {
            drop(owner);
            for _ in 0..40 {
                if let Some(reclaimed_owner) = open_stage_owner(&lock_path) {
                    if remove_abandoned_staging(&path) {
                        remove_stage_lock(&lock_path);
                    }
                    drop(reclaimed_owner);
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            tracing::warn!(path = %path.display(), "staging remains owned by running installer child");
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(cleanup);
        } else {
            cleanup();
        }
    }
}

pub struct LayerInstaller<'a> {
    pub uv_executable: &'a Path,
    pub python_executable: &'a Path,
    pub cache_directory: &'a Path,
    pub timeout: Duration,
}

pub fn valid_segment(name: &str) -> bool {
    !name.is_empty()
        && name.is_ascii()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn validate_lock(packages: &[LockedPackage], python_version: &str) -> Result<(), LayerError> {
    let parts: Vec<_> = python_version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(LayerError::Invalid("pin exact Python version".into()));
    }
    if packages.len() > 256 {
        return Err(LayerError::Invalid("package limit exceeded".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for p in packages {
        if p.name.is_empty()
            || !p
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
            || p.version.is_empty()
            || !p.version.bytes().next().unwrap().is_ascii_digit()
            || !p
                .version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'-' | b'_'))
        {
            return Err(LayerError::Invalid(
                "package name/version must be exact uv-safe pins".into(),
            ));
        }
        let normalized = p.name.to_ascii_lowercase().replace(['-', '_', '.'], "-");
        if !seen.insert(normalized) {
            return Err(LayerError::Invalid("duplicate distribution".into()));
        }
    }
    Ok(())
}

pub(super) fn reject_symlinked_layer_components(destination: &Path) -> Result<(), LayerError> {
    for component in destination.ancestors().take(3) {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LayerError::Invalid(
                    "layer revision or its managed parent must not be a symlink".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(LayerError::Io(error)),
        }
    }
    Ok(())
}

pub(super) fn reject_unsafe_layer_receipt(marker: &Path) -> Result<(), LayerError> {
    match std::fs::symlink_metadata(marker) {
        Ok(metadata) if !metadata.is_file() || metadata.len() > 131_072 => {
            Err(LayerError::Conflict)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LayerError::Io(error)),
    }
}

fn safe_marker_target(root: &Path, target: &Path) -> bool {
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    if !relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)))
    {
        return false;
    }
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component);
        if current.is_symlink() {
            return false;
        }
    }
    true
}

impl LayerInstaller<'_> {
    pub fn verify_paths(&self, destination: &Path) -> Result<(), LayerError> {
        if !destination.is_absolute()
            || !self.uv_executable.is_absolute()
            || !self.python_executable.is_absolute()
            || !self.cache_directory.is_absolute()
            || self.timeout.is_zero()
        {
            return Err(LayerError::Invalid(
                "absolute paths and nonzero timeout required".into(),
            ));
        }
        reject_symlinked_layer_components(destination)?;
        Ok(())
    }

    pub async fn installed<T: DeserializeOwned + PartialEq>(
        &self,
        destination: &Path,
        marker: &str,
        plan: &T,
        packages: &[LockedPackage],
        actions: &[InstallerAction],
    ) -> Result<bool, LayerError> {
        reject_symlinked_layer_components(destination)?;
        if !destination.exists() {
            return Ok(false);
        }
        let marker_path = destination.join(marker);
        reject_unsafe_layer_receipt(&marker_path)?;
        let data = tokio::fs::read(marker_path)
            .await
            .map_err(|_| LayerError::Conflict)?;
        let saved: T = serde_json::from_slice(&data).map_err(|_| LayerError::Conflict)?;
        if saved != *plan {
            return Err(LayerError::Conflict);
        }
        distribution::inspect(destination, packages).map_err(LayerError::Corrupted)?;
        for action in actions {
            let target = destination.join(&action.relative_path);
            if !safe_marker_target(destination, &target)
                || !tokio::fs::metadata(&target)
                    .await
                    .map_err(|_| LayerError::Corrupted(action.id.clone()))?
                    .is_file()
                || tokio::fs::metadata(&target).await?.len() != 0
            {
                return Err(LayerError::Corrupted(action.id.clone()));
            }
        }
        Ok(true)
    }

    pub async fn publish<T: Serialize + DeserializeOwned + PartialEq>(
        &self,
        destination: &Path,
        marker: &str,
        plan: &T,
        packages: &[LockedPackage],
        actions: &[InstallerAction],
    ) -> Result<PathBuf, LayerError> {
        self.verify_paths(destination)?;
        if self
            .installed(destination, marker, plan, packages, actions)
            .await?
        {
            return Ok(destination.to_owned());
        }
        if !self.uv_executable.is_file() || !self.python_executable.is_file() {
            return Err(LayerError::Invalid(
                "uv or managed Python is missing".into(),
            ));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| LayerError::Invalid("missing parent".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        let scan_parent = parent.to_owned();
        tokio::task::spawn_blocking(move || reclaim_abandoned_staging(&scan_parent))
            .await
            .map_err(|error| LayerError::Invalid(format!("staging recovery failed: {error}")))??;
        tokio::fs::create_dir_all(self.cache_directory).await?;
        let staging = parent.join(format!(".staging-{}", Uuid::new_v4()));
        let mut cleanup = StagingCleanup::new(staging.clone())?;
        async {
            if !packages.is_empty() {
                let mut cmd = Command::new(self.uv_executable);
                cmd.args([
                    "pip",
                    "install",
                    "--no-config",
                    "--no-deps",
                    "--no-python-downloads",
                    "--python",
                ])
                .arg(self.python_executable)
                .arg("--target")
                .arg(&staging)
                .env("UV_CACHE_DIR", self.cache_directory)
                .env_remove("VIRTUAL_ENV")
                .stdin(cleanup.child_stdin()?)
                .kill_on_drop(true);
                for package in packages {
                    cmd.arg(format!("{}=={}", package.name, package.version));
                }
                let outcome = timeout(self.timeout, cmd.output()).await;
                if matches!(&outcome, Ok(Ok(_))) {
                    cleanup.child_finished();
                }
                let output = outcome.map_err(|_| LayerError::Timeout)??;
                if !output.status.success() {
                    return Err(LayerError::Install(
                        String::from_utf8_lossy(&output.stderr)
                            .chars()
                            .take(1200)
                            .collect(),
                    ));
                }
            }
            distribution::inspect(&staging, packages).map_err(LayerError::Corrupted)?;
            for action in actions {
                if action.kind != "create_empty_marker" {
                    return Err(LayerError::Invalid("unsupported installer action".into()));
                }
                let target = staging.join(&action.relative_path);
                let parent = target
                    .parent()
                    .ok_or_else(|| LayerError::Invalid("invalid marker parent".into()))?;
                if !parent.is_dir() || !safe_marker_target(&staging, &target) || target.exists() {
                    return Err(LayerError::Invalid(
                        "marker location is not a pristine installed package".into(),
                    ));
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)
                    .await?;
                file.flush().await?;
            }
            let bytes =
                serde_json::to_vec_pretty(plan).map_err(|e| LayerError::Invalid(e.to_string()))?;
            tokio::fs::write(staging.join(marker), bytes).await?;
            if destination.exists() {
                if self
                    .installed(destination, marker, plan, packages, actions)
                    .await?
                {
                    return Ok(destination.to_owned());
                }
                return Err(LayerError::Conflict);
            }
            match std::fs::rename(&staging, destination) {
                Ok(()) => {
                    cleanup.disarm();
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    if self
                        .installed(destination, marker, plan, packages, actions)
                        .await?
                    {
                        return Ok(destination.to_owned());
                    }
                    return Err(LayerError::Conflict);
                }
                Err(error) => return Err(LayerError::Io(error)),
            }
            Ok(destination.to_owned())
        }
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_never_reclaims_orphan_without_proven_child_termination() {
        let root = std::env::temp_dir().join(format!("nm-windows-stage-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let abandoned = root.join(format!(".staging-{}", Uuid::new_v4()));
        std::fs::create_dir(&abandoned).unwrap();
        let marker = staging_lock_path(&abandoned);
        std::fs::write(&marker, []).unwrap();
        reclaim_abandoned_staging(&root).unwrap();
        assert!(abandoned.is_dir());
        assert!(marker.is_file());

        let pending = root.join(format!(".staging-{}", Uuid::new_v4()));
        let mut cleanup = StagingCleanup::new(pending.clone()).unwrap();
        let child = cleanup.child_stdin().unwrap();
        drop(child);
        drop(cleanup);
        assert!(pending.is_dir());
        assert!(staging_lock_path(&pending).is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn uv_child_survives_installer_parent_crash_fixture() {
        let Ok(root) = std::env::var("NEUROMITA_C3_PARENT_FIXTURE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let uv = root.join("uv");
        let python = root.join("python");
        let cache = root.join("cache");
        let installer = LayerInstaller {
            uv_executable: &uv,
            python_executable: &python,
            cache_directory: &cache,
            timeout: Duration::from_secs(60),
        };
        let packages = vec![LockedPackage {
            name: "mockpkg".into(),
            version: "1.0.0".into(),
        }];
        installer
            .publish(
                &root.join("layers").join("crashed"),
                ".receipt.json",
                &serde_json::json!({"revision": "crashed"}),
                &packages,
                &[],
            )
            .await
            .unwrap();
        panic!("mock uv must remain blocked until parent is killed");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn surviving_uv_child_prevents_orphan_reclamation_after_parent_is_killed() {
        use std::{
            os::unix::fs::PermissionsExt,
            process::{Child, Command as StdCommand},
        };

        struct ProcessGuard(Child);
        impl Drop for ProcessGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct ReleaseGuard(PathBuf);
        impl Drop for ReleaseGuard {
            fn drop(&mut self) {
                let _ = std::fs::write(&self.0, "release");
            }
        }

        let root = std::env::temp_dir().join(format!("nm-staging-uv-survival-{}", Uuid::new_v4()));
        let layers = root.join("layers");
        std::fs::create_dir_all(&layers).unwrap();
        let uv = root.join("uv");
        let python = root.join("python");
        std::fs::write(&python, "test Python executable").unwrap();
        std::fs::write(
            &uv,
            r#"#!/usr/bin/env python3
import os, pathlib, sys, time
stage = pathlib.Path(sys.argv[sys.argv.index('--target') + 1])
root = stage.parent.parent
(root / 'uv-ready').write_text(str(os.getpid()) + '\n' + str(stage))
while not (root / 'uv-release').exists():
    time.sleep(0.02)
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&uv).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&uv, perms).unwrap();

        let _release_guard = ReleaseGuard(root.join("uv-release"));
        let child = StdCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ai_runtime::layer::tests::uv_child_survives_installer_parent_crash_fixture",
                "--nocapture",
            ])
            .env("NEUROMITA_C3_PARENT_FIXTURE_ROOT", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut parent = ProcessGuard(child);
        let ready = root.join("uv-ready");
        tokio::time::timeout(Duration::from_secs(8), async {
            while !ready.is_file() {
                assert!(
                    parent.0.try_wait().unwrap().is_none(),
                    "installer parent exited unexpectedly"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("uv must start before killing installer parent");
        let data = std::fs::read_to_string(&ready).unwrap();
        let (pid, staging) = data.split_once('\n').unwrap();
        let pid: u32 = pid.parse().unwrap();
        let staging = PathBuf::from(staging);
        assert!(staging.parent() == Some(layers.as_path()));
        assert!(staging.is_dir());
        let lock = staging_lock_path(&staging);
        assert!(lock.is_file());

        parent.0.kill().unwrap();
        parent.0.wait().unwrap();
        assert!(
            PathBuf::from(format!("/proc/{pid}")).exists(),
            "uv child must survive its parent"
        );
        assert!(
            open_stage_owner(&lock).is_none(),
            "uv must inherit the owner's OS lock"
        );

        let cache = root.join("cache");
        let installer = LayerInstaller {
            uv_executable: &uv,
            python_executable: &python,
            cache_directory: &cache,
            timeout: Duration::from_secs(5),
        };
        let first = layers.join("healthy1");
        installer
            .publish(
                &first,
                ".receipt.json",
                &serde_json::json!({"revision": 1}),
                &[],
                &[],
            )
            .await
            .unwrap();
        assert!(
            staging.is_dir(),
            "live orphan uv staging must not be reclaimed"
        );
        assert!(open_stage_owner(&lock).is_none());

        std::fs::write(root.join("uv-release"), "release").unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            while open_stage_owner(&lock).is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("uv must release inherited ownership after exiting");

        let second = layers.join("healthy2");
        installer
            .publish(
                &second,
                ".receipt.json",
                &serde_json::json!({"revision": 2}),
                &[],
                &[],
            )
            .await
            .unwrap();
        assert!(
            !staging.exists(),
            "staging must become reclaimable after uv exit"
        );
        assert!(!lock.exists());
        assert!(first.is_dir() && second.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reclaims_crashed_staging_without_touching_another_process_install() {
        use std::process::{Child, Command as StdCommand, Stdio};

        struct OwnerProcess(Child);
        impl Drop for OwnerProcess {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        async fn spawn_owner(path: &Path) -> OwnerProcess {
            let script = r#"
import fcntl, os, sys, time
stage = sys.argv[1]
with open(stage + '.lock', 'xb') as owner:
    fcntl.flock(owner.fileno(), fcntl.LOCK_EX)
    os.makedirs(stage)
    with open(os.path.join(stage, 'ready'), 'w') as ready:
        ready.write('alive')
    while True:
        time.sleep(0.1)
"#;
            let child = StdCommand::new("python3")
                .arg("-c")
                .arg(script)
                .arg(path)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !path.join("ready").is_file() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("separate owner must lock its stage");
            OwnerProcess(child)
        }

        let root = std::env::temp_dir().join(format!("nm-staging-recovery-{}", Uuid::new_v4()));
        let parent = root.join("layers");
        std::fs::create_dir_all(&parent).unwrap();
        let live_stage = parent.join(format!(".staging-{}", Uuid::new_v4()));
        let dead_stage = parent.join(format!(".staging-{}", Uuid::new_v4()));
        let unowned_stage = parent.join(format!(".staging-{}", Uuid::new_v4()));
        let mut live = spawn_owner(&live_stage).await;
        let mut dead = spawn_owner(&dead_stage).await;
        dead.0.kill().unwrap();
        dead.0.wait().unwrap();
        std::fs::create_dir(&unowned_stage).unwrap();
        std::fs::write(unowned_stage.join("unowned"), "preserve").unwrap();

        let uv = root.join("uv");
        let python = root.join("python");
        std::fs::write(&uv, "fake uv").unwrap();
        std::fs::write(&python, "fake python").unwrap();
        let cache = root.join("cache");
        let installer = LayerInstaller {
            uv_executable: &uv,
            python_executable: &python,
            cache_directory: &cache,
            timeout: Duration::from_secs(3),
        };
        let spec = serde_json::json!({"revision": 1});
        let first = parent.join("rev1");
        installer
            .publish(&first, ".receipt.json", &spec, &[], &[])
            .await
            .unwrap();
        assert!(!dead_stage.exists(), "dead worker stage must be reclaimed");
        assert!(
            live_stage.join("ready").exists(),
            "active worker stage must survive"
        );
        assert!(
            unowned_stage.join("unowned").exists(),
            "unowned legacy stage must not be guessed dead"
        );
        assert!(
            !staging_lock_path(&first).exists(),
            "published layer must not carry stage lock"
        );

        live.0.kill().unwrap();
        live.0.wait().unwrap();
        let second = parent.join("rev2");
        installer
            .publish(&second, ".receipt.json", &spec, &[], &[])
            .await
            .unwrap();
        assert!(
            !live_stage.exists(),
            "recovery after owner exit must remove its stage"
        );
        assert!(
            installer
                .installed(&first, ".receipt.json", &spec, &[], &[])
                .await
                .unwrap()
        );
        assert!(
            installer
                .installed(&second, ".receipt.json", &spec, &[], &[])
                .await
                .unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
