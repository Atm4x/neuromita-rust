//! Shared read model and commands for Core-owned AI Hub installation.
//! All transport clients observe the same watch revision; no GUI event receiver.

use std::sync::Arc;
use tokio::sync::watch;
use uuid::Uuid;

use crate::ai_runtime::{
    install_admission::InstallAction,
    install_coordinator::{AiHubInstallCoordinator, InstallStatus},
    install_review::InstallPlanReview,
};

#[derive(Clone)]
pub struct AiHubInstallPresenter {
    owner: Arc<AiHubInstallCoordinator>,
}

#[derive(Clone, Debug)]
pub struct AiHubInstallView {
    pub category: String,
    pub id: String,
    pub token: i32,
    pub stage: String,
    pub summary: String,
    pub error: String,
    pub can_install: bool,
    pub operation_id: String,
    pub operation_summary: String,
}

impl AiHubInstallPresenter {
    pub fn new(owner: Arc<AiHubInstallCoordinator>) -> Self {
        Self { owner }
    }

    pub fn status(&self) -> InstallStatus {
        self.owner.status()
    }

    pub fn observe(&self) -> watch::Receiver<InstallStatus> {
        self.owner.observe_status()
    }

    pub async fn preview(&self, category: String, id: String, token: i32) {
        self.owner.preview(category, id, token).await;
    }

    pub async fn invalidate(&self) {
        self.owner.invalidate().await;
    }

    pub async fn confirm(
        &self,
        category: String,
        id: String,
        token: i32,
    ) -> Result<(Uuid, InstallAction), &'static str> {
        self.owner.confirm(category, id, token).await
    }

    pub fn project(status: &InstallStatus) -> AiHubInstallView {
        let stage = match (status.stage, status.action) {
            ("installed", Some("wheel")) => "wheel-installed",
            ("wheel_review", _) => "wheel-review",
            (other, _) => other,
        };
        let operation_summary = match (status.stage, status.action) {
            ("installed", Some("wheel")) => {
                "Один wheel-stage установлен и проверен. Модель и worker не активированы."
            }
            ("installed", _) => "Plugin overlay установлен и проверен. Worker не запущен.",
            ("installing", Some("wheel")) => "Установка одного проверенного wheel-stage",
            ("installing", _) => "Публикация проверенного plugin overlay",
            ("cancelled", _) => "Установка отменена.",
            ("failed", _) => {
                "Установка не завершена. Подробности доступны в диагностике установщика."
            }
            _ => "",
        };
        let summary = match (status.stage, status.action, &status.review) {
            ("review" | "wheel_review" | "not_installable", _, Some(review)) => {
                let mut summary = summarize_review(&review.review);
                if let Some(InstallAction::Wheel { stage_id, backend }) = &review.action {
                    summary.push_str(&format!("\nПроверенный wheel-stage: {stage_id} · {backend}. Устанавливается только этот пакет."));
                }
                if !status.can_install {
                    summary.push_str("\nВыполнение не разрешено: требуется отдельный проверенный этап установки.");
                }
                summary
            }
            ("installing", Some("wheel"), _) => {
                "Устанавливается один wheel-stage. Модель и worker не активируются.".into()
            }
            ("installing", _, _) => {
                "Установка plugin overlay выполняется. Worker не запускается.".into()
            }
            ("installed", _, _) => operation_summary.into(),
            ("cancelled", _, _) => "Установка отменена.".into(),
            _ => String::new(),
        };
        let error = match status.error_code {
            Some("invalid_selection") => "Модель недоступна или неоднозначна в актуальном каталоге",
            Some("sdk_unavailable") => "Для проверки необходим проверенный SDK и код плагина",
            Some("python_unavailable") => "Managed CPython недоступен или не прошёл проверку",
            Some("invalid_plan") => "Установочный план не прошёл проверку",
            Some("review_changed") => "План изменился после подтверждения",
            Some("unsupported_install") => "Для этого плана необходим отдельный установщик",
            Some("wheel_pin" | "wheel_execution") => {
                "Проверенный wheel-stage недоступен либо не выполнен"
            }
            Some("cancelled") => "Установка отменена",
            Some(_) => "Core не смог завершить установку. Смотрите журнал установщика",
            None => "",
        };
        AiHubInstallView {
            category: status.category.clone(),
            id: status.id.clone(),
            token: status.token,
            stage: stage.into(),
            summary,
            error: error.into(),
            can_install: status.can_install,
            operation_id: status
                .operation_id
                .map_or_else(String::new, |id| id.to_string()),
            operation_summary: operation_summary.into(),
        }
    }
}

fn summarize_review(review: &InstallPlanReview) -> String {
    let mut lines = vec![
        format!(
            "Python: {} · SDK и источник проверены",
            review.identity.python_version
        ),
        format!("Backend: {}", review.backend_options.join(", ")),
        format!(
            "Пакетов: {} · требований: {}",
            review.packages.len(),
            review.requirements.len()
        ),
        format!(
            "Файлов моделей: обязательных {}, дополнительных {}",
            review.required_assets.len(),
            review.optional_assets.len()
        ),
        format!(
            "Действий с отдельным подтверждением: {} · hooks: {}",
            review.actions.len(),
            review.asset_hooks.len()
        ),
        format!(
            "Build-stage: {} (без автоматического исполнения)",
            review.build_stages.len()
        ),
        format!(
            "Отпечаток плана: {}",
            review.fingerprint.chars().take(16).collect::<String>()
        ),
        "План проверен, но установка ещё не разрешена и не запускалась.".to_owned(),
    ];
    if !review.required_backends.is_empty() {
        lines.insert(
            2,
            format!(
                "Дополнительные backend: {}",
                review.required_backends.join(", ")
            ),
        );
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn observers_are_not_exclusive_and_projection_is_stable() {
        let root = std::env::temp_dir().join(format!("ai-hub-observers-{}", Uuid::new_v4()));
        let presenter = AiHubInstallPresenter::new(AiHubInstallCoordinator::new(root));
        let mut gui = presenter.observe();
        let mut mcp = presenter.observe();
        presenter
            .preview("unknown".into(), "unavailable".into(), 4)
            .await;
        gui.changed().await.unwrap();
        mcp.changed().await.unwrap();
        assert_eq!(
            gui.borrow_and_update().revision,
            mcp.borrow_and_update().revision
        );
        assert_eq!(
            AiHubInstallPresenter::project(&presenter.status()).stage,
            "failed"
        );
    }
}
