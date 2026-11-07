//! Explicitly approved Python model conversions. Only staged, verified outputs become active assets.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::{Duration, Instant},
};

use serde_json::Value;
use thiserror::Error;
use tokio::{io::AsyncReadExt, process::Command, sync::oneshot, time::timeout};
use uuid::Uuid;

use super::{
    AssetReceipt, ModelAssetError, ModelAssetStore, RECEIPT, VerifiedAsset, hash_file,
    reject_symlinks,
};
#[cfg(target_os = "linux")]
use crate::ai_runtime::linux_worker_lifetime;
#[cfg(windows)]
use crate::ai_runtime::windows_uv_job;
use crate::ai_runtime::{install::PluginInstallPlan, revision_lease};

pub struct ModelGenerationRequest<'a> {
    pub plugin_id: &'a str,
    pub revision: &'a str,
    pub plan: &'a PluginInstallPlan,
    pub python_version: &'a str,
    pub hook_id: &'a str,
    pub source_plugin_id: &'a str,
    pub source_revision: &'a str,
    pub settings: &'a Value,
    pub python_executable: &'a Path,
    pub sdk_root: &'a Path,
    pub plugins_directory: &'a Path,
    pub dependency_layers: &'a [PathBuf],
    pub approved_hook_id: Option<&'a str>,
}

#[derive(Debug, Error)]
pub enum ModelGenerationError {
    #[error("asset generation was not explicitly approved")]
    NotApproved,
    #[error("invalid model generation request: {0}")]
    Invalid(String),
    #[error("Python installer hook failed: {0}")]
    Hook(String),
    #[error("Python installer hook timed out")]
    Timeout,
    #[error(transparent)]
    Assets(#[from] ModelAssetError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ModelAssetStore {
    pub fn publish_generated<'a>(
        &'a self,
        request: ModelGenerationRequest<'a>,
    ) -> impl std::future::Future<Output = Result<PathBuf, ModelGenerationError>> + Send + 'a {
        Box::pin(self.generate(request))
    }

    async fn generate(
        &self,
        request: ModelGenerationRequest<'_>,
    ) -> Result<PathBuf, ModelGenerationError> {
        let destination = self.revision_path(request.plugin_id, request.revision)?;
        if request.approved_hook_id != Some(request.hook_id) {
            return Err(ModelGenerationError::NotApproved);
        }
        request
            .plan
            .validate(request.python_version)
            .map_err(|error| ModelGenerationError::Invalid(error.to_string()))?;
        if !request.plan.asset_hooks.iter().any(|hook| {
            hook.id == request.hook_id && hook.source_plugin_id == request.source_plugin_id
        }) || request.plan.assets.is_empty()
            || !request.settings.is_object()
            || !request.python_executable.is_absolute()
            || !request.sdk_root.is_absolute()
            || !request.plugins_directory.is_absolute()
            || request
                .dependency_layers
                .iter()
                .any(|path| !path.is_absolute())
        {
            return Err(ModelGenerationError::Invalid(
                "unrecognized hook, assets or runtime paths".into(),
            ));
        }
        let source = self
            .verified_revision(request.source_plugin_id, request.source_revision)
            .await?;
        if source == destination {
            return Err(ModelGenerationError::Invalid(
                "source and target revisions must differ".into(),
            ));
        }
        reject_symlinks(&destination).await?;
        reject_symlinks(request.sdk_root).await?;
        reject_symlinks(request.plugins_directory).await?;
        if destination.exists() {
            return Err(ModelGenerationError::Assets(ModelAssetError::Conflict));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| ModelGenerationError::Invalid("missing target parent".into()))?;
        tokio::fs::create_dir_all(parent).await?;
        let staging = parent.join(format!(".staging-{}", Uuid::new_v4()));
        let inputs = staging.join("input");
        let outputs = staging.join("output");
        tokio::fs::create_dir_all(&inputs).await?;
        tokio::fs::create_dir(&outputs).await?;

        let result = async {
            let receipt: AssetReceipt =
                serde_json::from_slice(&tokio::fs::read(source.join(RECEIPT)).await?)
                    .map_err(|error| ModelGenerationError::Invalid(error.to_string()))?;
            for asset in &receipt.assets {
                let original = source.join(&asset.relative_path);
                let copy = inputs.join(&asset.relative_path);
                tokio::fs::create_dir_all(
                    copy.parent().ok_or_else(|| {
                        ModelGenerationError::Invalid("invalid source path".into())
                    })?,
                )
                .await?;
                tokio::fs::copy(original, &copy).await?;
                if !hash_file(&copy, self.max_asset_bytes)
                    .await?
                    .eq_ignore_ascii_case(&asset.sha256)
                {
                    return Err(ModelGenerationError::Invalid(
                        "source changed during staging".into(),
                    ));
                }
            }
            let settings = serde_json::to_string(request.settings)
                .map_err(|error| ModelGenerationError::Invalid(error.to_string()))?;
            if settings.len() > 65536 {
                return Err(ModelGenerationError::Invalid(
                    "install settings too large".into(),
                ));
            }
            let paths = std::iter::once(request.sdk_root.to_path_buf())
                .chain(request.dependency_layers.iter().cloned())
                .collect::<Vec<_>>();
            let python_path = std::env::join_paths(&paths)
                .map_err(|error| ModelGenerationError::Invalid(error.to_string()))?;
            let mut command = Command::new(request.python_executable);
            #[cfg(windows)]
            windows_uv_job::gated_module(&mut command, "neuromita_ai_sdk.installer");
            #[cfg(not(windows))]
            command.arg("-m").arg("neuromita_ai_sdk.installer");
            command
                .arg("--python-version")
                .arg(request.python_version)
                .arg("--plugins-dir")
                .arg(request.plugins_directory)
                .arg("--plugin-id")
                .arg(request.plugin_id)
                .arg("--hook-id")
                .arg(request.hook_id)
                .arg("--source-plugin-id")
                .arg(request.source_plugin_id)
                .arg("--source-root")
                .arg(&inputs)
                .arg("--output-root")
                .arg(&outputs)
                .arg("--settings")
                .arg(settings)
                .env("PYTHONPATH", python_path)
                .env("PYTHONNOUSERSITE", "1")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .env_remove("PYTHONHOME")
                .stdin(if cfg!(windows) {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let output = run_install_hook(command, request.dependency_layers, self.timeout).await?;
            if !output.status.success() {
                let message = String::from_utf8_lossy(&output.stderr);
                return Err(ModelGenerationError::Hook(
                    message.chars().take(1200).collect(),
                ));
            }
            let mut expected = HashSet::new();
            let mut produced = Vec::new();
            for asset in &request.plan.assets {
                let path = outputs.join(&asset.relative_path);
                expected.insert(asset.relative_path.clone());
                reject_symlinks(&path).await?;
                match tokio::fs::symlink_metadata(&path).await {
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound && !asset.required =>
                    {
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(ModelGenerationError::Invalid(format!(
                            "missing generated asset: {}",
                            asset.relative_path
                        )));
                    }
                    Err(error) => return Err(error.into()),
                    Ok(metadata) if !metadata.is_file() || metadata.len() == 0 => {
                        return Err(ModelGenerationError::Invalid(format!(
                            "invalid generated asset: {}",
                            asset.relative_path
                        )));
                    }
                    Ok(_) => (),
                }
                let digest = hash_file(&path, self.max_asset_bytes).await?;
                if let Some(pinned) = &asset.sha256
                    && !digest.eq_ignore_ascii_case(pinned)
                {
                    return Err(ModelGenerationError::Invalid(format!(
                        "hash mismatch for {}",
                        asset.relative_path
                    )));
                }
                produced.push(VerifiedAsset {
                    role: asset.role.clone(),
                    relative_path: asset.relative_path.clone(),
                    sha256: digest,
                });
            }
            let mut folders = vec![outputs.clone()];
            while let Some(folder) = folders.pop() {
                let mut listing = tokio::fs::read_dir(&folder).await?;
                while let Some(entry) = listing.next_entry().await? {
                    let path = entry.path();
                    let metadata = tokio::fs::symlink_metadata(&path).await?;
                    if metadata.file_type().is_symlink() {
                        return Err(ModelGenerationError::Invalid(
                            "generated symlink is forbidden".into(),
                        ));
                    }
                    if metadata.is_dir() {
                        folders.push(path);
                        continue;
                    }
                    let relative = path
                        .strip_prefix(&outputs)
                        .map_err(|_| ModelGenerationError::Invalid("invalid output path".into()))?;
                    let name = relative
                        .components()
                        .map(|part| {
                            part.as_os_str().to_str().ok_or_else(|| {
                                ModelGenerationError::Invalid("non-UTF8 output path".into())
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .join("/");
                    if !metadata.is_file() || !expected.contains(&name) {
                        return Err(ModelGenerationError::Invalid(
                            "undeclared generated asset".into(),
                        ));
                    }
                }
            }
            self.verified_revision(request.source_plugin_id, request.source_revision)
                .await?;
            let receipt = AssetReceipt {
                plugin_id: request.plugin_id.into(),
                revision: request.revision.into(),
                assets: produced,
            };
            tokio::fs::write(
                outputs.join(RECEIPT),
                serde_json::to_vec_pretty(&receipt)
                    .map_err(|error| ModelGenerationError::Invalid(error.to_string()))?,
            )
            .await?;
            if destination.exists() {
                return Err(ModelGenerationError::Assets(ModelAssetError::Conflict));
            }
            tokio::fs::rename(&outputs, &destination).await?;
            Ok(destination)
        }
        .await;
        let _ = tokio::fs::remove_dir_all(staging).await;
        result
    }
}

// Cancellation must not release a revision lease before the subprocess has
// actually stopped. The detached task owns both the leases and the child.
struct CancelHook(Option<oneshot::Sender<()>>);

impl Drop for CancelHook {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

async fn run_install_hook(
    mut command: Command,
    dependency_layers: &[PathBuf],
    max_duration: Duration,
) -> Result<Output, ModelGenerationError> {
    let leases = revision_lease::shared_layers(dependency_layers)?;
    command.kill_on_drop(true);
    #[cfg(target_os = "linux")]
    linux_worker_lifetime::require_parent_death_kill(&mut command);

    let (sender, mut cancellation) = oneshot::channel();
    let _cancel_on_drop = CancelHook(Some(sender));
    let handle =
        tokio::spawn(async move {
            let _leases = leases;
            let mut child = command.spawn()?;
            #[cfg(windows)]
            let job = windows_uv_job::attach_and_activate(&mut child).await?;
            #[cfg(windows)]
            drop(child.stdin.take());
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("Python installer stdout pipe unavailable"))?;
            let mut stderr = child
                .stderr
                .take()
                .ok_or_else(|| std::io::Error::other("Python installer stderr pipe unavailable"))?;
            let stdout_reader = tokio::spawn(async move {
                let mut data = Vec::new();
                stdout.read_to_end(&mut data).await.map(|_| data)
            });
            let stderr_reader = tokio::spawn(async move {
                let mut data = Vec::new();
                stderr.read_to_end(&mut data).await.map(|_| data)
            });
            let started = Instant::now();
            let result = async {
        let status = tokio::select! {
            outcome = child.wait() => outcome?,
            _ = tokio::time::sleep(max_duration) => {
                let _ = child.start_kill();
                child.wait().await?;
                return Err(ModelGenerationError::Timeout);
            }
            _ = &mut cancellation => {
                let _ = child.start_kill();
                child.wait().await?;
                return Err(ModelGenerationError::Invalid("installer hook cancelled".into()));
            }
        };
        let remaining = max_duration.saturating_sub(started.elapsed());
        let (stdout, stderr) = timeout(remaining, async {
            let stdout = stdout_reader.await.map_err(std::io::Error::other)??;
            let stderr = stderr_reader.await.map_err(std::io::Error::other)??;
            Ok::<_, std::io::Error>((stdout, stderr))
        })
        .await
        .map_err(|_| ModelGenerationError::Timeout)??;
        Ok(Output { status, stdout, stderr })
        }.await;
            #[cfg(windows)]
            {
                let job_result = tokio::task::spawn_blocking(move || {
                    job.terminate_and_wait(Duration::from_secs(10))
                })
                .await
                .map_err(std::io::Error::other)??;
                if !job_result {
                    return Err(ModelGenerationError::Invalid(
                        "Python installer job did not become empty".into(),
                    ));
                }
            }
            result
        });
    handle
        .await
        .map_err(|error| ModelGenerationError::Invalid(format!("installer task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::assets::{AssetInput, AssetSource};
    use crate::ai_runtime::install::{AssetHook, ModelAssetRequirement};
    use crate::ai_runtime::overlay::LockedPackage;
    use std::time::Duration;

    fn layer_fixture() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("nm-hook-lease-{}", Uuid::new_v4()));
        let revision = root.join("dependencies").join("r1");
        std::fs::create_dir_all(&revision).unwrap();
        (root, revision)
    }

    fn python_command(script: &str, marker: &Path) -> Command {
        let interpreter = std::env::var("NEUROMITA_TEST_PYTHON").unwrap_or_else(|_| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let mut command = Command::new(interpreter);
        #[cfg(windows)]
        windows_uv_job::gated_script(&mut command, script);
        #[cfg(not(windows))]
        command.arg("-c").arg(script);
        command
            .arg(marker)
            .stdin(if cfg!(windows) {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    async fn marker_ready(marker: &Path) {
        timeout(Duration::from_secs(8), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Python installer did not reach the marker");
    }

    #[tokio::test]
    async fn exclusive_owner_denies_hook_before_any_python_execution() {
        let (root, revision) = layer_fixture();
        let marker = root.join("hook_was_executed");
        let writer = revision_lease::RevisionLease::exclusive(&revision).unwrap();
        let command = python_command(
            "import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('ran')",
            &marker,
        );
        assert!(matches!(
            run_install_hook(command, &[revision.clone()], Duration::from_secs(5)).await,
            Err(ModelGenerationError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(
            !marker.exists(),
            "hook executed without owning its revision"
        );
        drop(writer);
        let command = python_command(
            "import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('ran')",
            &marker,
        );
        assert!(
            run_install_hook(command, &[revision], Duration::from_secs(5))
                .await
                .unwrap()
                .status
                .success()
        );
        assert!(marker.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn live_installer_hook_holds_shared_dependency_leases_until_exit() {
        let (root, revision) = layer_fixture();
        let marker = root.join("hook_is_running");
        let cmd = python_command(
            "import pathlib,sys,time;pathlib.Path(sys.argv[1]).write_text('ready');time.sleep(1)",
            &marker,
        );
        let owned_revision = revision.clone();
        let handle = tokio::spawn(async move {
            run_install_hook(cmd, &[owned_revision], Duration::from_secs(8)).await
        });
        marker_ready(&marker).await;
        assert_eq!(
            revision_lease::RevisionLease::exclusive(&revision)
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(handle.await.unwrap().unwrap().status.success());
        assert!(revision_lease::RevisionLease::exclusive(&revision).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelled_hook_is_reaped_before_dependency_lease_is_released() {
        let (root, revision) = layer_fixture();
        let marker = root.join("hook_pid");
        let cmd = python_command(
            "import os,pathlib,sys,time;pathlib.Path(sys.argv[1]).write_text(str(os.getpid()));time.sleep(60)",
            &marker,
        );
        let owned_revision = revision.clone();
        let handle = tokio::spawn(async move {
            run_install_hook(cmd, &[owned_revision], Duration::from_secs(65)).await
        });
        marker_ready(&marker).await;
        let pid = std::fs::read_to_string(&marker).unwrap();
        handle.abort();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match revision_lease::RevisionLease::exclusive(&revision) {
                Ok(writer) => {
                    drop(writer);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "cancellation leaked the hook lease"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("unexpected lease failure: {error}"),
            }
        }
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "cancelled hook released its lease while Python was still alive"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn windows_hook_owner_crash_fixture() {
        let Ok(marker) = std::env::var("NM_WINDOWS_HOOK_OWNER_MARKER") else {
            return;
        };
        let (root, revision) = layer_fixture();
        let command = python_command(
            "import os,pathlib,subprocess,sys,time; child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); pathlib.Path(sys.argv[1]).write_text(str(os.getpid())+' '+str(child.pid)); time.sleep(120)",
            Path::new(&marker),
        );
        let _ = run_install_hook(command, &[revision], Duration::from_secs(125)).await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn terminated_rust_hook_owner_kills_python_and_descendant() {
        use std::{
            fs,
            os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::{Command as ProcessCommand, Stdio},
        };
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{OpenProcess, WaitForSingleObject},
        };

        const SYNCHRONIZE_PROCESS: u32 = 0x0010_0000;
        let root = std::env::temp_dir().join(format!("nm-win-hook-owner-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let marker = root.join("python-and-child.pid");
        let mut owner = ProcessCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ai_runtime::assets::generation::tests::windows_hook_owner_crash_fixture",
            ])
            .env("NM_WINDOWS_HOOK_OWNER_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "Python hook did not launch");
            assert!(
                owner.try_wait().unwrap().is_none(),
                "hook owner exited early"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let pids: Vec<u32> = fs::read_to_string(&marker)
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        let handles: Vec<OwnedHandle> = pids
            .iter()
            .map(|pid| {
                let raw = unsafe { OpenProcess(SYNCHRONIZE_PROCESS, 0, *pid) };
                assert!(!raw.is_null(), "Python process {pid} not running");
                unsafe { OwnedHandle::from_raw_handle(raw.cast()) }
            })
            .collect();
        for handle in &handles {
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 0) },
                WAIT_TIMEOUT,
                "Python hook or descendant exited before owner termination"
            );
        }
        owner.kill().unwrap();
        owner.wait().unwrap();
        for handle in &handles {
            assert_eq!(
                unsafe { WaitForSingleObject(handle.as_raw_handle().cast(), 10_000) },
                WAIT_OBJECT_0,
                "orphan Python hook or descendant survived Rust owner termination"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn plan(role: &str, path: &str, hooks: bool) -> PluginInstallPlan {
        PluginInstallPlan {
            packages: Vec::<LockedPackage>::new(),
            requirements: vec![],
            assets: vec![ModelAssetRequirement {
                role: role.into(),
                relative_path: path.into(),
                required: true,
                sha256: None,
            }],
            backend: None,
            supported_backends: vec![],
            required_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
            asset_hooks: if hooks {
                vec![AssetHook {
                    id: "convert".into(),
                    source_plugin_id: "input_model".into(),
                }]
            } else {
                vec![]
            },
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approved_installer_stages_verifies_and_publishes_only_declared_assets() {
        let root = std::env::temp_dir().join(format!("nm-generate-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let binary = root.join("source.ckpt");
        tokio::fs::write(&binary, b"checkpoint").await.unwrap();
        let store = ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 4096,
            timeout: Duration::from_secs(12),
        };
        let input = plan("checkpoint", "source.ckpt", false);
        let sha = hash_file(&binary, 4096).await.unwrap();
        let source = [AssetInput {
            role: "checkpoint".into(),
            relative_path: "source.ckpt".into(),
            sha256: sha,
            source: AssetSource::LocalFile(binary.clone()),
        }];
        store
            .publish("input_model", "r1", &input, &source)
            .await
            .unwrap();
        let plugin = root.join("plugins").join("fixture");
        tokio::fs::create_dir_all(&plugin).await.unwrap();
        tokio::fs::write(plugin.join("plugin.json"), r#"{"id":"fixture","name":"Fixture","kind":"asr","api_version":1,"entrypoint":"plugin.py"}"#).await.unwrap();
        tokio::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n")
            .await
            .unwrap();
        tokio::fs::write(plugin.join("installplan.py"), r#"from neuromita_ai_sdk import AssetHook, AssetRequirement, InstallPlan
class InstallContract:
    def install_plan(self): return InstallPlan(assets=(AssetRequirement('export','converted.onnx'),), asset_hooks=(AssetHook('convert','input_model'),))
    def install_plan_for_settings(self, settings): return self.install_plan()
    def probe_install(self): return {}
    def settings_schema(self): return {}
"#).await.unwrap();
        tokio::fs::write(plugin.join("install.py"), r#"def execute(hook_id, source_root, output_root, settings):
    assert hook_id == 'convert'
    (output_root / 'converted.onnx').write_bytes(b'onnx:' + (source_root / 'source.ckpt').read_bytes())
"#).await.unwrap();
        let python = std::env::var("NEUROMITA_TEST_PYTHON").unwrap_or_else(|_| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let output = Command::new(&python)
            .arg("-c")
            .arg("import sys;print(sys.executable)")
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let python = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let version = Command::new(&python)
            .arg("--version")
            .output()
            .await
            .unwrap();
        assert!(version.status.success());
        let python_version = String::from_utf8_lossy(&version.stdout)
            .trim()
            .strip_prefix("Python ")
            .unwrap()
            .to_owned();
        assert!(
            python_version.split('.').count() == 3,
            "fixture requires a three-part managed Python version"
        );
        let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let plan = plan("export", "converted.onnx", true);
        let settings = serde_json::json!({"model":"v2_ctc"});
        let plugins_root = root.join("plugins");
        macro_rules! gen_req {
            ($rev:expr, $approval:expr) => {
                ModelGenerationRequest {
                    plugin_id: "fixture",
                    revision: $rev,
                    plan: &plan,
                    python_version: &python_version,
                    hook_id: "convert",
                    source_plugin_id: "input_model",
                    source_revision: "r1",
                    settings: &settings,
                    python_executable: &python,
                    sdk_root: &sdk,
                    plugins_directory: &plugins_root,
                    dependency_layers: &[],
                    approved_hook_id: $approval,
                }
            };
        }
        assert!(matches!(
            Box::pin(store.publish_generated(gen_req!("r1", None))).await,
            Err(ModelGenerationError::NotApproved)
        ));
        assert!(!root.join("models/fixture/r1").exists());
        let mut wrong_python = gen_req!("wrong-python", Some("convert"));
        wrong_python.python_version = "3.12.999";
        assert!(
            matches!(store.publish_generated(wrong_python).await, Err(ModelGenerationError::Hook(message)) if message.contains("locked Python version"))
        );
        assert!(!root.join("models/fixture/wrong-python").exists());
        let published = store
            .publish_generated(gen_req!("r1", Some("convert")))
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(published.join("converted.onnx"))
                .await
                .unwrap(),
            b"onnx:checkpoint"
        );
        store.verified_revision("fixture", "r1").await.unwrap();
        assert!(matches!(
            store
                .publish_generated(gen_req!("r1", Some("convert")))
                .await,
            Err(ModelGenerationError::Assets(ModelAssetError::Conflict))
        ));
        tokio::fs::write(
            plugin.join("install.py"),
            r#"def execute(hook_id, source_root, output_root, settings):
    (output_root / 'converted.onnx').write_bytes(b'onnx:checkpoint')
    (output_root / 'unlisted.bin').write_bytes(b'extra')
"#,
        )
        .await
        .unwrap();
        assert!(
            matches!(Box::pin(store.publish_generated(gen_req!("r2", Some("convert")))).await, Err(ModelGenerationError::Invalid(message)) if message.contains("undeclared"))
        );
        assert!(!root.join("models/fixture/r2").exists());
        assert_eq!(
            tokio::fs::read(published.join("converted.onnx"))
                .await
                .unwrap(),
            b"onnx:checkpoint"
        );
        tokio::fs::write(root.join("models/input_model/r1/source.ckpt"), b"corrupted")
            .await
            .unwrap();
        assert!(matches!(
            store
                .publish_generated(gen_req!("r3", Some("convert")))
                .await,
            Err(ModelGenerationError::Assets(ModelAssetError::Conflict))
        ));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
