//! Core-owned installation admission and immutable overlay publication.
//! Preview stays read-only in install_review; execution is a separate boundary.

use serde::Deserialize;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::{
    AiService,
    build_stage::{ApprovedWheelStage, WheelStageInstaller},
    install::{BuildStageKind, PluginInstallHandler, PluginInstallPlan, PluginInstallRequest},
    install_review::{InstallPlanReview, InstallPreviewError, inspect_installed_environment},
    layer::{sha256_file, valid_segment},
    overlay::LockedPackage,
    overlay::OverlayInstaller,
    resolver::DependencyResolver,
};

/// One-use Core admission gate. The UI never transports a serialized plan or
/// retains permission to install after navigation, refresh, or another review.
#[derive(Default)]
pub struct InstallApprovalGate {
    pending: Option<(i32, InstallPlanReview, InstallIntent)>,
}

#[derive(Clone, PartialEq, Eq)]
enum InstallIntent {
    Overlay,
    Wheel { stage_id: String, backend: String },
}

impl InstallApprovalGate {
    pub fn offer(&mut self, token: i32, review: InstallPlanReview) -> bool {
        self.pending =
            review
                .eligible_overlay_install()
                .then_some((token, review, InstallIntent::Overlay));
        self.pending.is_some()
    }

    pub fn consume(
        &mut self,
        token: i32,
        service: AiService,
        plugin_id: &str,
    ) -> Option<InstallPlanReview> {
        let (issued_token, review, intent) = self.pending.take()?;
        (intent == InstallIntent::Overlay
            && issued_token == token
            && review.identity.service == service
            && review.identity.plugin_id == plugin_id)
            .then_some(review)
    }

    pub fn offer_wheel(
        &mut self,
        token: i32,
        review: InstallPlanReview,
        stage_id: &str,
        backend: &str,
    ) -> bool {
        self.pending = None;
        if !valid_segment(stage_id) || !valid_segment(backend) || review.build_stages.is_empty() {
            return false;
        }
        self.pending = Some((
            token,
            review,
            InstallIntent::Wheel {
                stage_id: stage_id.to_owned(),
                backend: backend.to_owned(),
            },
        ));
        true
    }

    pub fn consume_wheel(
        &mut self,
        token: i32,
        service: AiService,
        plugin_id: &str,
        stage_id: &str,
        backend: &str,
    ) -> Option<InstallPlanReview> {
        let (issued_token, review, intent) = self.pending.take()?;
        (issued_token == token
            && review.identity.service == service
            && review.identity.plugin_id == plugin_id
            && intent
                == InstallIntent::Wheel {
                    stage_id: stage_id.to_owned(),
                    backend: backend.to_owned(),
                })
        .then_some(review)
    }

    pub fn invalidate(&mut self) {
        self.pending = None;
    }
}

/// Publish only a reviewed, builtin remote-API plugin overlay without model assets,
/// supplemental backends, hooks or installer actions. Core re-executes and validates
/// the plan, then consumes the exact bytes from that admission, never an UI payload.
/// This does not initialize or activate an inference worker.
pub async fn install_reviewed_overlay(
    data_directory: &Path,
    expected: &InstallPlanReview,
) -> Result<PathBuf, InstallPreviewError> {
    if !expected.eligible_overlay_install() {
        return Err(InstallPreviewError::UnsupportedInstall);
    }
    let evaluated = inspect_installed_environment(
        data_directory,
        expected.identity.service,
        &expected.identity.plugin_id,
        &Value::Object(Default::default()),
    )
    .await?;
    if evaluated.review != *expected {
        return Err(InstallPreviewError::ReviewChanged);
    }
    if !evaluated.review.eligible_overlay_install()
        || !overlay_only_plan(&evaluated.plan)
        || !evaluated.plan.assets.is_empty()
        || !evaluated.plan.actions.is_empty()
        || !evaluated.plan.asset_hooks.is_empty()
        || !evaluated.plan.required_backends.is_empty()
        || !evaluated.plan.packages.is_empty()
        || !evaluated.plan.requirements.is_empty()
    {
        return Err(InstallPreviewError::UnsupportedInstall);
    }
    let ai_data = data_directory.join("AIData");
    let handler = PluginInstallHandler {
        resolver: DependencyResolver {
            uv_executable: evaluated.uv.clone(),
            python_executable: evaluated.python.clone(),
            cache_directory: ai_data.join("cache/uv"),
            timeout: Duration::from_secs(300),
        },
        overlays: OverlayInstaller {
            uv_executable: evaluated.uv,
            python_executable: evaluated.python,
            overlays_directory: ai_data.join("overlays"),
            cache_directory: ai_data.join("cache/uv"),
            timeout: Duration::from_secs(300),
        },
        assets_directory: ai_data.join("models"),
    };
    let revision = format!("sha256-{}", evaluated.review.identity.plugin_revision);
    let selected_backend = "remote-api";
    let result = handler
        .install_with_approval(
            PluginInstallRequest {
                plugin_id: &expected.identity.plugin_id,
                revision: &revision,
                python_version: &expected.identity.python_version,
                selected_backend: Some(selected_backend),
                plan: &evaluated.plan,
                backend_packages: &[],
            },
            &Default::default(),
        )
        .await?;
    Ok(result.overlay_path)
}

fn overlay_only_plan(plan: &PluginInstallPlan) -> bool {
    plan.backend.as_deref() == Some("remote-api")
        || plan.backend.is_none() && plan.supported_backends == ["remote-api"]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuiltinWheelLock {
    stages: Vec<BuiltinWheelPin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuiltinWheelPin {
    id: String,
    backend: String,
    package: LockedPackage,
    wheel_file: String,
    wheel_sha256: String,
}

/// Execute one independently verified wheel stage. This publishes only an immutable
/// build-stage layer, not a plugin overlay, backend activation, or a ready worker.
/// All wheel pins are loaded from the reviewed builtin plugin source, never from UI.
pub async fn install_reviewed_wheel_stage(
    data_directory: &Path,
    expected: &InstallPlanReview,
    stage_id: &str,
    backend: &str,
) -> Result<PathBuf, InstallPreviewError> {
    if !valid_segment(stage_id) || !valid_segment(backend) {
        return Err(InstallPreviewError::WheelPin);
    }
    let evaluated = inspect_installed_environment(
        data_directory,
        expected.identity.service,
        &expected.identity.plugin_id,
        &Value::Object(Default::default()),
    )
    .await?;
    if evaluated.review != *expected {
        return Err(InstallPreviewError::ReviewChanged);
    }
    let mut stages = evaluated
        .plan
        .build_stages
        .iter()
        .filter(|stage| stage.id == stage_id && stage.backend == backend);
    let stage = stages.next().ok_or(InstallPreviewError::WheelPin)?;
    if stages.next().is_some() || stage.kind != BuildStageKind::PythonWheel {
        return Err(InstallPreviewError::WheelPin);
    }
    let contents = evaluated
        .wheel_lock
        .as_deref()
        .ok_or(InstallPreviewError::WheelPin)?;
    let lock: BuiltinWheelLock =
        serde_json::from_slice(contents).map_err(|_| InstallPreviewError::WheelPin)?;
    if lock.stages.is_empty() || lock.stages.len() > 16 {
        return Err(InstallPreviewError::WheelPin);
    }
    let mut pins = lock
        .stages
        .iter()
        .filter(|entry| entry.id == stage.id && entry.backend == stage.backend);
    let pin = pins.next().ok_or(InstallPreviewError::WheelPin)?;
    if pins.next().is_some() {
        return Err(InstallPreviewError::WheelPin);
    }
    let requirement = format!("{}=={}", pin.package.name, pin.package.version);
    if stage.requirement.as_deref() != Some(requirement.as_str())
        || !evaluated
            .plan
            .requirements
            .iter()
            .any(|value| value == &requirement)
    {
        return Err(InstallPreviewError::WheelPin);
    }
    let ai_data = data_directory.join("AIData");
    let approved = ApprovedWheelStage {
        plugin_id: expected.identity.plugin_id.clone(),
        plugin_revision: expected.identity.plugin_revision.clone(),
        python_version: expected.identity.python_version.clone(),
        build_stage: stage.clone(),
        package: pin.package.clone(),
        wheel_file: pin.wheel_file.clone(),
        wheel_sha256: pin.wheel_sha256.clone(),
        install_plan_sha256: expected.fingerprint.clone(),
        uv_sha256: sha256_file(&evaluated.uv).map_err(|_| InstallPreviewError::WheelPin)?,
        python_sha256: sha256_file(&evaluated.python).map_err(|_| InstallPreviewError::WheelPin)?,
    };
    approved
        .validate()
        .map_err(|_| InstallPreviewError::WheelPin)?;
    WheelStageInstaller {
        uv_executable: evaluated.uv,
        python_executable: evaluated.python,
        data_directory: ai_data,
        timeout: Duration::from_secs(300),
    }
    .install(&approved)
    .await
    .map_err(|_| InstallPreviewError::WheelExecution)
}

#[cfg(test)]
mod wheel_admission_tests {
    use super::*;
    use crate::ai_runtime::{
        install::PluginInstallPlan,
        install_review::{InstallReviewIdentity, preview_installed_environment},
    };
    use serde_json::json;

    fn wheel_review() -> InstallPlanReview {
        let plan: PluginInstallPlan = serde_json::from_value(json!({
            "backend": "onnx-cpu",
            "requirements": ["sample_pkg==1.2.3"],
            "build_stages": [{
                "id": "locked",
                "kind": "python_wheel",
                "backend": "onnx-cpu",
                "platform": "linux",
                "requirement": "sample_pkg==1.2.3"
            }]
        }))
        .unwrap();
        InstallPlanReview::new(
            InstallReviewIdentity {
                service: AiService::Asr,
                plugin_id: "fixture_asr".into(),
                plugin_revision: "testrev".into(),
                sdk_revision: format!("sha256-{}", "c".repeat(64)),
                python_version: "3.12.10".into(),
            },
            &json!({}),
            &plan,
        )
        .unwrap()
    }

    #[test]
    fn wheel_approval_is_single_use_and_does_not_authorize_overlay() {
        let mut gate = InstallApprovalGate::default();
        let review = wheel_review();
        assert!(!review.eligible_overlay_install());
        assert!(gate.offer_wheel(71, review.clone(), "locked", "onnx-cpu"));
        assert!(gate.consume(71, AiService::Asr, "fixture_asr").is_none());
        assert!(
            gate.consume_wheel(71, AiService::Asr, "fixture_asr", "locked", "onnx-cpu")
                .is_none()
        );
        assert!(gate.offer_wheel(72, review.clone(), "locked", "onnx-cpu"));
        assert!(
            gate.consume_wheel(72, AiService::Tts, "fixture_asr", "locked", "onnx-cpu")
                .is_none()
        );
        assert!(gate.offer_wheel(73, review.clone(), "locked", "onnx-cpu"));
        assert!(
            gate.consume_wheel(73, AiService::Asr, "fixture_asr", "locked", "torch-cuda")
                .is_none()
        );
        assert!(gate.offer_wheel(74, review.clone(), "locked", "onnx-cpu"));
        assert_eq!(
            gate.consume_wheel(74, AiService::Asr, "fixture_asr", "locked", "onnx-cpu"),
            Some(review)
        );
        assert!(
            gate.consume_wheel(74, AiService::Asr, "fixture_asr", "locked", "onnx-cpu")
                .is_none()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn reviewed_builtin_wheel_is_pinned_to_source_and_never_optimistically_installed() {
        use crate::ai_runtime::sdk::SdkPublisher;
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("nm-reviewed-wheel-{}", uuid::Uuid::new_v4()));
        let plugin = root.join("AIData/plugins/builtin/fixture_asr");
        let python_dir = root.join("AIData/bootstrap/python/cpython-3.12.10-fixture/bin");
        let uv_dir = root.join("AIData/bootstrap/uv/rev-fixture");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::create_dir_all(&python_dir).unwrap();
        std::fs::create_dir_all(&uv_dir).unwrap();
        std::fs::write(plugin.join("plugin.json"), r#"{"id":"fixture_asr","name":"Fixture","kind":"asr","entrypoint":"plugin.py","api_version":1}"#).unwrap();
        std::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n").unwrap();
        std::fs::write(plugin.join("installplan.py"), r#"
from neuromita_ai_sdk import InstallPlan, BuildStage
class InstallContract:
    def install_plan(self):
        return InstallPlan(backend='onnx-cpu', requirements=('sample_pkg==1.2.3',), build_stages=(BuildStage('locked', 'python_wheel', 'onnx-cpu', 'linux', 'sample_pkg==1.2.3'),))
    def settings_schema(self): return {}
    def probe_install(self): return {'installed': False, 'missing': []}
"#).unwrap();
        let lock_path = plugin.join("wheel-stages.lock.json");
        let lock = format!(
            r#"{{"stages":[{{"id":"locked","backend":"onnx-cpu","package":{{"name":"sample_pkg","version":"1.2.3"}},"wheel_file":"sample_pkg-1.2.3-py3-none-any.whl","wheel_sha256":"{}"}}]}}"#,
            "a".repeat(64)
        );
        std::fs::write(&lock_path, &lock).unwrap();
        let python = python_dir.join("python3");
        std::fs::write(&python, "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 'Python 3.12.10'; exit 0; fi\nexec python3 \"$@\"\n").unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let uv = uv_dir.join("uv");
        std::fs::write(&uv, "#!/bin/sh\nif [ \"$1\" = 'python' ] && [ \"$2\" = 'find' ]; then printf '%s\\n' \"$UV_PYTHON_INSTALL_DIR/cpython-3.12.10-fixture/bin/python3\"; exit 0; fi\nexit 3\n").unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        SdkPublisher::new(root.join("AIData"))
            .publish_embedded()
            .unwrap();

        let review =
            preview_installed_environment(&root, AiService::Asr, "fixture_asr", &json!({}))
                .await
                .unwrap();
        assert_eq!(review.build_stages.len(), 1);
        assert!(matches!(
            install_reviewed_wheel_stage(&root, &review, "locked", "torch-cuda").await,
            Err(InstallPreviewError::WheelPin)
        ));
        assert!(matches!(
            install_reviewed_wheel_stage(&root, &review, "locked", "onnx-cpu").await,
            Err(InstallPreviewError::WheelExecution)
        ));
        assert!(!root.join("AIData/build-stages").exists());
        assert!(!root.join("AIData/overlays").exists());
        std::fs::write(&lock_path, lock.replace(&"a".repeat(64), &"b".repeat(64))).unwrap();
        assert!(matches!(
            install_reviewed_wheel_stage(&root, &review, "locked", "onnx-cpu").await,
            Err(InstallPreviewError::ReviewChanged)
        ));
        std::fs::write(&lock_path, lock).unwrap();
        std::fs::write(plugin.join("plugin.py"), "class Plugin: pass\n# changed\n").unwrap();
        assert!(matches!(
            install_reviewed_wheel_stage(&root, &review, "locked", "onnx-cpu").await,
            Err(InstallPreviewError::ReviewChanged)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
