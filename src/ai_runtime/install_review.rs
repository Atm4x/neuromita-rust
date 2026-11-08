//! Immutable, validated installation-plan review. The review never executes a plan.

use ring::digest::{Context, SHA256, digest};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

use super::{
    AiService, PythonHostConfig,
    bootstrap::{PythonBootstrap, PythonBootstrapState},
    fetch_install_plan_for,
    install::{PluginInstallError, PluginInstallPlan},
    installed_speech::InstalledSpeechEnvironment,
    sdk::SdkPublisher,
    speech_catalog::{executable_plugin_root, verify_executable_plugin},
};

#[derive(Debug, Default, Clone, Copy)]
pub struct InstallReviewFence {
    request: u64,
    catalog_generation: u64,
}

impl InstallReviewFence {
    pub fn begin(&mut self) -> (u64, u64) {
        self.request = self.request.wrapping_add(1);
        (self.request, self.catalog_generation)
    }

    pub fn catalog_replaced(&mut self) {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.request = self.request.wrapping_add(1);
    }

    pub fn accepts(&self, request: u64, generation: u64) -> bool {
        request == self.request && generation == self.catalog_generation
    }
}

#[derive(Debug, Error)]
pub enum InstallPreviewError {
    #[error("выбранная модель недоступна или неоднозначна")]
    InvalidSelection,
    #[error("проверенный Python SDK ещё не установлен: {0}")]
    Sdk(String),
    #[error("требуется ровно одна установленная версия managed CPython 3.12.x")]
    PythonVersion,
    #[error("установленный managed Python недоступен: {0}")]
    Python(String),
    #[error("установочный план плагина недоступен: {0}")]
    Plan(String),
    #[error("неверный установочный план: {0}")]
    InvalidPlan(#[from] PluginInstallError),
    #[error("план изменился после подтверждения")]
    ReviewChanged,
    #[error("для этого плана пока требуется отдельный установщик backend или моделей")]
    UnsupportedInstall,
    #[error("зафиксированный wheel-stage отсутствует или не соответствует выбранному плану")]
    WheelPin,
    #[error("проверенный wheel-stage не удалось выполнить")]
    WheelExecution,
}

/// Request an actual Python plugin install plan using *only existing* SDK and Python.
/// This executes the plugin's install-contract Python code. It performs no Rust-owned
/// installation or bootstrap, and is not an OS sandbox for untrusted Python code.
/// The result is review data, never an authorization to install.
pub async fn preview_installed_environment(
    data_directory: &Path,
    service: AiService,
    plugin_id: &str,
    settings: &Value,
) -> Result<InstallPlanReview, InstallPreviewError> {
    Ok(
        inspect_installed_environment(data_directory, service, plugin_id, settings)
            .await?
            .review,
    )
}

pub(super) struct EvaluatedInstall {
    pub(super) review: InstallPlanReview,
    pub(super) plan: PluginInstallPlan,
    pub(super) uv: PathBuf,
    pub(super) python: PathBuf,
    pub(super) wheel_lock: Option<Vec<u8>>,
}

pub(super) async fn inspect_installed_environment(
    data_directory: &Path,
    service: AiService,
    plugin_id: &str,
    settings: &Value,
) -> Result<EvaluatedInstall, InstallPreviewError> {
    let service_name = match service {
        AiService::Tts => "tts",
        AiService::Asr => "asr",
        _ => return Err(InstallPreviewError::InvalidSelection),
    };
    if !data_directory.is_absolute()
        || !super::layer::valid_segment(plugin_id)
        || !settings.is_object()
    {
        return Err(InstallPreviewError::InvalidSelection);
    }
    let plugins = executable_plugin_root(data_directory);
    if !plugins.starts_with(data_directory) {
        return Err(InstallPreviewError::Sdk(
            "portable plugin files are not installed".into(),
        ));
    }
    verify_executable_plugin(&plugins, plugin_id, service_name)
        .map_err(|_| InstallPreviewError::InvalidSelection)?;
    let plugin_directory = [
        plugins.join("builtin").join(plugin_id),
        plugins.join("community").join(plugin_id),
        plugins.join(plugin_id),
    ]
    .into_iter()
    .find(|path| path.join("plugin.json").is_file())
    .ok_or(InstallPreviewError::InvalidSelection)?;
    // Do not execute a community plugin's arbitrary Python code at the review stage.
    if plugin_directory
        .parent()
        .is_some_and(|parent| parent.ends_with("community"))
    {
        return Err(InstallPreviewError::InvalidSelection);
    }
    let manifest = read_review_source(&plugin_directory.join("plugin.json"))?;
    let script = if plugin_directory.join("installplan.py").exists() {
        plugin_directory.join("installplan.py")
    } else {
        plugin_directory.join("plugin.py")
    };
    let install_script = read_review_source(&script)?;
    let wheel_lock_path = plugin_directory.join("wheel-stages.lock.json");
    let wheel_lock = match std::fs::symlink_metadata(&wheel_lock_path) {
        Ok(_) => Some(read_review_source(&wheel_lock_path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(InstallPreviewError::InvalidSelection),
    };
    let entrypoint = plugin_directory.join("plugin.py");
    let entrypoint_bytes = read_review_source(&entrypoint)?;
    let mut source = Context::new(&SHA256);
    for bytes in [&manifest, &entrypoint_bytes, &install_script] {
        source.update(&bytes.len().to_le_bytes());
        source.update(bytes);
    }
    if let Some(lock) = wheel_lock.as_ref() {
        source.update(&lock.len().to_le_bytes());
        source.update(lock);
    }
    let plugin_revision = fingerprint_bytes(source.finish().as_ref());
    let ai_data = data_directory.join("AIData");
    let sdk_root = SdkPublisher::new(ai_data.clone())
        .resolve_active()
        .map_err(|error| InstallPreviewError::Sdk(error.to_string()))?;
    let sdk_revision = sdk_root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| InstallPreviewError::Sdk("SDK revision is missing".into()))?
        .to_owned();
    let python_version = installed_python_version(&ai_data)?;
    let environment = InstalledSpeechEnvironment::for_desktop(data_directory.to_path_buf());
    let uv = environment
        .installed_uv()
        .map_err(|error| InstallPreviewError::Python(error.to_string()))?;
    let mut bootstrap = PythonBootstrap::new(uv.clone(), ai_data.clone(), python_version.clone());
    bootstrap.command_timeout = Duration::from_secs(20);
    let python = match bootstrap
        .probe()
        .await
        .map_err(|error| InstallPreviewError::Python(error.to_string()))?
    {
        PythonBootstrapState::Ready(path) => path,
        _ => {
            return Err(InstallPreviewError::Python(
                "managed Python not ready".into(),
            ));
        }
    };
    let version_output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(&python)
            .arg("--version")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| InstallPreviewError::Python("interpreter version probe timed out".into()))?
    .map_err(|_| InstallPreviewError::Python("interpreter version probe failed".into()))?;
    let detected = if version_output.stdout.is_empty() {
        String::from_utf8_lossy(&version_output.stderr)
            .trim()
            .to_string()
    } else {
        String::from_utf8_lossy(&version_output.stdout)
            .trim()
            .to_string()
    };
    if !version_output.status.success() || detected != format!("Python {python_version}") {
        return Err(InstallPreviewError::Python(
            "unexpected managed interpreter version".into(),
        ));
    }
    let host = PythonHostConfig {
        python_executable: python.clone(),
        sdk_root,
        plugins_directory: plugins,
        models_directory: Some(ai_data.join("models")),
        request_timeout: Duration::from_secs(25),
    };
    let plan = fetch_install_plan_for(&host, plugin_id, settings.clone())
        .await
        .map_err(|error| InstallPreviewError::Plan(error.to_string()))?;
    let review = InstallPlanReview::new(
        InstallReviewIdentity {
            service,
            plugin_id: plugin_id.to_owned(),
            plugin_revision,
            sdk_revision,
            python_version,
        },
        settings,
        &plan,
    )
    .map_err(InstallPreviewError::from)?;
    if read_review_source(&plugin_directory.join("plugin.json"))? != manifest
        || read_review_source(&entrypoint)? != entrypoint_bytes
        || read_review_source(&script)? != install_script
        || wheel_lock
            .as_ref()
            .is_some_and(|lock| {
                !matches!(read_review_source(&wheel_lock_path), Ok(observed) if &observed == lock)
            })
        || wheel_lock.is_none() && std::fs::symlink_metadata(&wheel_lock_path).is_ok()
    {
        return Err(InstallPreviewError::ReviewChanged);
    }
    Ok(EvaluatedInstall {
        review,
        plan,
        uv,
        python,
        wheel_lock,
    })
}

fn read_review_source(path: &Path) -> Result<Vec<u8>, InstallPreviewError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| InstallPreviewError::InvalidSelection)?;
    if !metadata.is_file() || metadata.is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(InstallPreviewError::InvalidSelection);
    }
    std::fs::read(path).map_err(|_| InstallPreviewError::InvalidSelection)
}

fn fingerprint_bytes(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn installed_python_version(ai_data: &Path) -> Result<String, InstallPreviewError> {
    let entries = std::fs::read_dir(ai_data.join("bootstrap/python"))
        .map_err(|_| InstallPreviewError::PythonVersion)?;
    let mut found = Vec::new();
    for entry in entries.take(128).flatten() {
        if !entry
            .file_type()
            .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(tail) = name.strip_prefix("cpython-3.12.") else {
            continue;
        };
        let Some((patch, _platform)) = tail.split_once('-') else {
            continue;
        };
        if patch.is_empty() || !patch.bytes().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        found.push(format!("3.12.{patch}"));
    }
    if found.len() != 1 {
        return Err(InstallPreviewError::PythonVersion);
    }
    Ok(found.remove(0))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReviewIdentity {
    pub service: AiService,
    pub plugin_id: String,
    pub plugin_revision: String,
    pub sdk_revision: String,
    pub python_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlanReview {
    pub identity: InstallReviewIdentity,
    pub fingerprint: String,
    pub packages: Vec<String>,
    pub requirements: Vec<String>,
    pub backend_options: Vec<String>,
    pub required_backends: Vec<String>,
    pub required_assets: Vec<String>,
    pub optional_assets: Vec<String>,
    pub actions: Vec<String>,
    pub asset_hooks: Vec<String>,
    pub build_stages: Vec<String>,
}

impl InstallPlanReview {
    /// Only backend-independent, asset-free remote API overlays are admitted for
    /// the initial Core-backed installation path. Other plans remain review-only.
    pub fn eligible_overlay_install(&self) -> bool {
        self.backend_options == ["remote-api"]
            && self.packages.is_empty()
            && self.requirements.is_empty()
            && self.required_backends.is_empty()
            && self.required_assets.is_empty()
            && self.optional_assets.is_empty()
            && self.actions.is_empty()
            && self.asset_hooks.is_empty()
            && self.build_stages.is_empty()
    }

    pub fn new(
        identity: InstallReviewIdentity,
        settings: &Value,
        plan: &PluginInstallPlan,
    ) -> Result<Self, PluginInstallError> {
        if !matches!(identity.service, AiService::Tts | AiService::Asr)
            || !super::layer::valid_segment(&identity.plugin_id)
            || !super::layer::valid_segment(&identity.plugin_revision)
            || !identity.sdk_revision.starts_with("sha256-")
            || identity.sdk_revision.len() != 71
            || !identity.sdk_revision[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !settings.is_object()
        {
            return Err(PluginInstallError::Invalid(
                "invalid installation review context".into(),
            ));
        }
        plan.validate(&identity.python_version)?;
        if plan.backend.is_none() && plan.supported_backends.is_empty() {
            return Err(PluginInstallError::Invalid(
                "install plan declares no backend".into(),
            ));
        }
        let serialized = serde_json::to_vec(&(
            &identity.plugin_id,
            identity.service.kind(),
            &identity.plugin_revision,
            &identity.sdk_revision,
            &identity.python_version,
            settings,
            plan,
        ))
        .map_err(|_| {
            PluginInstallError::Invalid("cannot fingerprint installation review".into())
        })?;
        let fingerprint = digest(&SHA256, &serialized)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let (required_assets, optional_assets) = plan.assets.iter().fold(
            (Vec::new(), Vec::new()),
            |(mut required, mut optional), asset| {
                let label = format!("{}: {}", asset.role, asset.relative_path);
                if asset.required {
                    required.push(label);
                } else {
                    optional.push(label);
                }
                (required, optional)
            },
        );
        Ok(Self {
            identity,
            fingerprint,
            packages: plan
                .packages
                .iter()
                .map(|pkg| format!("{} {}", pkg.name, pkg.version))
                .collect(),
            requirements: plan.requirements.clone(),
            backend_options: plan
                .backend
                .iter()
                .cloned()
                .chain(plan.supported_backends.iter().cloned())
                .collect(),
            required_backends: plan.required_backends.clone(),
            required_assets,
            optional_assets,
            actions: plan
                .actions
                .iter()
                .map(|action| format!("{} ({})", action.id, action.kind))
                .collect(),
            asset_hooks: plan
                .asset_hooks
                .iter()
                .map(|hook| hook.id.clone())
                .collect(),
            build_stages: plan
                .build_stages
                .iter()
                .map(|stage| format!("{} ({:?}, {:?})", stage.id, stage.kind, stage.platform))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::install::{ModelAssetRequirement, PluginInstallPlan};
    use crate::ai_runtime::install_admission::{InstallApprovalGate, install_reviewed_overlay};
    use serde_json::json;

    fn identity() -> InstallReviewIdentity {
        InstallReviewIdentity {
            service: AiService::Tts,
            plugin_id: "demo_tts".into(),
            plugin_revision: "r1".into(),
            sdk_revision: format!("sha256-{}", "a".repeat(64)),
            python_version: "3.12.10".into(),
        }
    }

    fn plan() -> PluginInstallPlan {
        PluginInstallPlan {
            build_stages: vec![],
            backend: Some("onnx-cpu".into()),
            assets: vec![ModelAssetRequirement {
                role: "weights".into(),
                relative_path: "weights.onnx".into(),
                required: true,
                sha256: None,
            }],
            ..serde_json::from_value(json!({})).unwrap()
        }
    }

    #[test]
    fn async_install_review_rejects_superseded_and_refreshed_catalog() {
        let mut fence = InstallReviewFence::default();
        let first = fence.begin();
        let second = fence.begin();
        assert!(!fence.accepts(first.0, first.1));
        assert!(fence.accepts(second.0, second.1));
        fence.catalog_replaced();
        assert!(!fence.accepts(second.0, second.1));
        let next = fence.begin();
        assert!(fence.accepts(next.0, next.1));
    }

    #[test]
    fn review_lists_real_install_requirements() {
        let review = InstallPlanReview::new(identity(), &json!({}), &plan()).unwrap();
        assert_eq!(review.backend_options, ["onnx-cpu"]);
        assert_eq!(review.required_assets, ["weights: weights.onnx"]);
        assert!(review.optional_assets.is_empty());
        assert_eq!(review.fingerprint.len(), 64);
    }

    #[test]
    fn settings_source_and_sdk_revision_bind_fingerprint() {
        let initial = InstallPlanReview::new(identity(), &json!({"voice":"en"}), &plan()).unwrap();
        let changed_settings =
            InstallPlanReview::new(identity(), &json!({"voice":"ru"}), &plan()).unwrap();
        let mut changed_source = identity();
        changed_source.plugin_revision = "r2".into();
        let changed_source =
            InstallPlanReview::new(changed_source, &json!({"voice":"en"}), &plan()).unwrap();
        let mut changed_sdk = identity();
        changed_sdk.sdk_revision = format!("sha256-{}", "b".repeat(64));
        let changed_sdk =
            InstallPlanReview::new(changed_sdk, &json!({"voice":"en"}), &plan()).unwrap();
        assert_ne!(initial.fingerprint, changed_settings.fingerprint);
        assert_ne!(initial.fingerprint, changed_source.fingerprint);
        assert_ne!(initial.fingerprint, changed_sdk.fingerprint);
    }

    #[test]
    fn managed_python_version_requires_exact_single_attested_installation() {
        let root = std::env::temp_dir().join(format!("ai-plan-python-{}", uuid::Uuid::new_v4()));
        let python = root.join("bootstrap/python");
        std::fs::create_dir_all(python.join("cpython-3.12.10-linux-x64")).unwrap();
        assert_eq!(installed_python_version(&root).unwrap(), "3.12.10");
        std::fs::create_dir_all(python.join("cpython-3.12.11-linux-x64")).unwrap();
        assert!(matches!(
            installed_python_version(&root),
            Err(InstallPreviewError::PythonVersion)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn preview_rejects_missing_portable_sdk_without_bootstrap() {
        let root = std::env::temp_dir().join(format!("ai-plan-empty-{}", uuid::Uuid::new_v4()));
        let plugin = root.join("AIData/plugins/builtin/demo_asr");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"id":"demo_asr","name":"Demo","kind":"asr","entrypoint":"plugin.py","api_version":1}"#).unwrap();
        std::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n").unwrap();
        let before = std::fs::read_dir(&root).unwrap().count();
        assert!(matches!(
            preview_installed_environment(&root, AiService::Asr, "demo_asr", &json!({})).await,
            Err(InstallPreviewError::Sdk(_))
        ));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), before);
        assert!(!root.join("AIData/bootstrap").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_sdk_plan_is_read_and_validated_without_installing_any_overlay() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ai-plan-fixture-{}", uuid::Uuid::new_v4()));
        let plugin = root.join("AIData/plugins/builtin/demo_asr");
        let python_dir = root.join("AIData/bootstrap/python/cpython-3.12.10-fixture/bin");
        let uv_dir = root.join("AIData/bootstrap/uv/rev-fixture");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::create_dir_all(&python_dir).unwrap();
        std::fs::create_dir_all(&uv_dir).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"id":"demo_asr","name":"Demo","kind":"asr","entrypoint":"plugin.py","api_version":1}"#).unwrap();
        std::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n").unwrap();
        std::fs::write(plugin.join("installplan.py"), r#"
from neuromita_ai_sdk import InstallPlan, AssetRequirement
class InstallContract:
    def install_plan(self):
        return InstallPlan(backend='onnx-cpu', assets=(AssetRequirement(role='weights', relative_path='model.onnx'),))
    def settings_schema(self): return {}
    def probe_install(self): return {'installed': False, 'missing': ['weights']}
"#).unwrap();
        let wrapped_python = python_dir.join("python3");
        std::fs::write(&wrapped_python, "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'Python 3.12.10'; exit 0; fi\nexec python3 \"$@\"\n").unwrap();
        std::fs::set_permissions(&wrapped_python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let uv = uv_dir.join("uv");
        std::fs::write(&uv, "#!/bin/sh\nif [ \"$1\" = 'python' ] && [ \"$2\" = 'find' ]; then printf '%s\\n' \"$UV_PYTHON_INSTALL_DIR/cpython-3.12.10-fixture/bin/python3\"; exit 0; fi\nexit 3\n").unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::SdkPublisher::new(root.join("AIData"))
            .publish_embedded()
            .unwrap();
        let review = preview_installed_environment(&root, AiService::Asr, "demo_asr", &json!({}))
            .await
            .unwrap();
        assert_eq!(review.backend_options, ["onnx-cpu"]);
        assert_eq!(review.required_assets, ["weights: model.onnx"]);
        assert!(!root.join("AIData/overlays").exists());
        assert!(!root.join("AIData/backends").exists());
        assert!(!root.join("AIData/models").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_stages_are_reviewed_but_not_automatically_approved() {
        let mut with_jit = plan();
        with_jit.backend = Some("remote-api".into());
        with_jit.assets.clear();
        with_jit.build_stages = vec![super::super::install::BuildStage {
            id: "needs_compilation".into(),
            kind: super::super::install::BuildStageKind::RuntimeJit,
            backend: "remote-api".into(),
            platform: super::super::install::BuildPlatform::Linux,
            requirement: None,
        }];
        let review = InstallPlanReview::new(identity(), &json!({}), &with_jit).unwrap();
        assert_eq!(review.build_stages.len(), 1);
        assert!(!review.eligible_overlay_install());
        let mut gate = InstallApprovalGate::default();
        assert!(!gate.offer(1, review));
    }

    #[test]
    fn explicit_install_gate_is_one_use_and_scoped_to_model() {
        let mut simple = plan();
        simple.backend = Some("remote-api".into());
        simple.assets.clear();
        let review = InstallPlanReview::new(identity(), &json!({}), &simple).unwrap();
        let mut gate = InstallApprovalGate::default();
        assert!(gate.offer(23, review.clone()));
        assert!(gate.consume(24, AiService::Tts, "demo_tts").is_none());
        assert!(gate.consume(23, AiService::Tts, "demo_tts").is_none());
        assert!(gate.offer(23, review.clone()));
        assert!(gate.consume(23, AiService::Asr, "demo_tts").is_none());
        assert!(gate.offer(23, review.clone()));
        assert!(gate.consume(23, AiService::Tts, "other").is_none());
        assert!(gate.offer(23, review.clone()));
        assert_eq!(gate.consume(23, AiService::Tts, "demo_tts"), Some(review));
        assert!(gate.consume(23, AiService::Tts, "demo_tts").is_none());
        let mut no_assets = plan();
        no_assets.backend = Some("onnx-cpu".into());
        no_assets.assets.clear();
        assert!(!gate.offer(
            24,
            InstallPlanReview::new(identity(), &json!({}), &no_assets).unwrap()
        ));
    }

    #[test]
    fn only_remote_api_no_assets_or_actions_can_install_from_review() {
        let mut simple = plan();
        simple.backend = Some("remote-api".into());
        simple.assets.clear();
        let review = InstallPlanReview::new(identity(), &json!({}), &simple).unwrap();
        assert!(review.eligible_overlay_install());
        simple.required_backends.push("torch-cpu".into());
        assert!(
            !InstallPlanReview::new(identity(), &json!({}), &simple)
                .unwrap()
                .eligible_overlay_install()
        );
        simple.required_backends.clear();
        simple.packages.push(super::super::overlay::LockedPackage {
            name: "somepkg".into(),
            version: "1.0".into(),
        });
        assert!(
            !InstallPlanReview::new(identity(), &json!({}), &simple)
                .unwrap()
                .eligible_overlay_install()
        );
        simple.packages.clear();
        simple.assets.push(ModelAssetRequirement {
            role: "weights".into(),
            relative_path: "model.bin".into(),
            required: false,
            sha256: None,
        });
        assert!(
            !InstallPlanReview::new(identity(), &json!({}), &simple)
                .unwrap()
                .eligible_overlay_install()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approved_builtin_remote_overlay_publishes_once_and_rejects_changed_plan() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("ai-install-approved-{}", uuid::Uuid::new_v4()));
        let plugin = root.join("AIData/plugins/builtin/demo_asr");
        let python_dir = root.join("AIData/bootstrap/python/cpython-3.12.10-fixture/bin");
        let uv_dir = root.join("AIData/bootstrap/uv/rev-fixture");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::create_dir_all(&python_dir).unwrap();
        std::fs::create_dir_all(&uv_dir).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"id":"demo_asr","name":"Demo","kind":"asr","entrypoint":"plugin.py","api_version":1}"#).unwrap();
        std::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n").unwrap();
        let script = plugin.join("installplan.py");
        std::fs::write(
            &script,
            r#"
from neuromita_ai_sdk import InstallPlan
class InstallContract:
    def install_plan(self): return InstallPlan(backend='remote-api')
    def settings_schema(self): return {}
    def probe_install(self): return {'installed': False, 'missing': []}
"#,
        )
        .unwrap();
        let wrapped_python = python_dir.join("python3");
        std::fs::write(&wrapped_python, "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'Python 3.12.10'; exit 0; fi\nexec python3 \"$@\"\n").unwrap();
        std::fs::set_permissions(&wrapped_python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let uv = uv_dir.join("uv");
        std::fs::write(&uv, "#!/bin/sh\nif [ \"$1\" = 'python' ] && [ \"$2\" = 'find' ]; then printf '%s\\n' \"$UV_PYTHON_INSTALL_DIR/cpython-3.12.10-fixture/bin/python3\"; exit 0; fi\nexit 3\n").unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        SdkPublisher::new(root.join("AIData"))
            .publish_embedded()
            .unwrap();
        let settings = json!({});
        let review = preview_installed_environment(&root, AiService::Asr, "demo_asr", &settings)
            .await
            .unwrap();
        assert!(review.eligible_overlay_install());
        assert!(!root.join("AIData/overlays").exists());
        let location = install_reviewed_overlay(&root, &review).await.unwrap();
        assert!(location.join(".neuromita-overlay.json").is_file());
        assert_eq!(
            install_reviewed_overlay(&root, &review).await.unwrap(),
            location
        );
        std::fs::write(
            plugin.join("plugin.py"),
            "class Plugin: pass\n# changed after approval\n",
        )
        .unwrap();
        assert!(matches!(
            install_reviewed_overlay(&root, &review).await,
            Err(InstallPreviewError::ReviewChanged)
        ));
        std::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n").unwrap();
        std::fs::write(&script, r#"
from neuromita_ai_sdk import InstallPlan, AssetRequirement
class InstallContract:
    def install_plan(self): return InstallPlan(backend='remote-api', assets=(AssetRequirement(role='weights', relative_path='model.bin'),))
    def settings_schema(self): return {}
    def probe_install(self): return {'installed': False, 'missing': ['model.bin']}
"#).unwrap();
        assert!(matches!(
            install_reviewed_overlay(&root, &review).await,
            Err(InstallPreviewError::ReviewChanged)
        ));
        assert!(location.join(".neuromita-overlay.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_ambiguous_or_malformed_contracts() {
        let mut no_backend = plan();
        no_backend.backend = None;
        assert!(InstallPlanReview::new(identity(), &json!({}), &no_backend).is_err());
        let mut bad_asset = plan();
        bad_asset.assets[0].relative_path = "../other-file".into();
        assert!(InstallPlanReview::new(identity(), &json!({}), &bad_asset).is_err());
        assert!(InstallPlanReview::new(identity(), &json!([]), &plan()).is_err());
        let mut bad_id = identity();
        bad_id.plugin_id = "../oops".into();
        assert!(InstallPlanReview::new(bad_id, &json!({}), &plan()).is_err());
    }
}
