//! Core-owned installation admission and immutable overlay publication.
//! Preview stays read-only in install_review; execution is a separate boundary.

use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::{
    AiService,
    install::{PluginInstallHandler, PluginInstallPlan, PluginInstallRequest},
    install_review::{InstallPlanReview, InstallPreviewError, inspect_installed_environment},
    overlay::OverlayInstaller,
    resolver::DependencyResolver,
};

/// One-use Core admission gate. The UI never transports a serialized plan or
/// retains permission to install after navigation, refresh, or another review.
#[derive(Default)]
pub struct InstallApprovalGate {
    pending: Option<(i32, InstallPlanReview)>,
}

impl InstallApprovalGate {
    pub fn offer(&mut self, token: i32, review: InstallPlanReview) -> bool {
        self.pending = review.eligible_overlay_install().then_some((token, review));
        self.pending.is_some()
    }

    pub fn consume(
        &mut self,
        token: i32,
        service: AiService,
        plugin_id: &str,
    ) -> Option<InstallPlanReview> {
        let (issued_token, review) = self.pending.take()?;
        (issued_token == token
            && review.identity.service == service
            && review.identity.plugin_id == plugin_id)
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
