//! Core-owned admission and supervised execution for AI Hub installation.
//! The desktop renders events but never owns an approved plan or an installer task.

use std::{path::PathBuf, sync::Arc};

use serde::Serialize;
use tokio::{
    sync::{Mutex as AsyncMutex, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    AiService,
    install_admission::{
        InstallAction, InstallActionReview, InstallApprovalGate, install_reviewed_overlay,
        install_reviewed_wheel_stage, preview_install_action,
    },
    install_review::{InstallPreviewError, InstallReviewFence},
    installation_owner::{InstallComponentKind, InstallationOwner, InstallationTarget},
    speech_catalog::SpeechModelCatalog,
};

#[derive(Clone, Debug, Serialize)]
pub struct InstallStatus {
    pub revision: u64,
    pub category: String,
    pub id: String,
    pub token: i32,
    pub stage: &'static str,
    pub can_install: bool,
    pub operation_id: Option<Uuid>,
    pub action: Option<&'static str>,
    pub error_code: Option<&'static str>,
    /// Immutable preview DTO retained for an in-process Presenter projection.
    /// The raw plan is never serialized in the public IPC status.
    #[serde(skip)]
    pub review: Option<InstallActionReview>,
}

impl Default for InstallStatus {
    fn default() -> Self {
        Self {
            revision: 0,
            category: String::new(),
            id: String::new(),
            token: 0,
            stage: "idle",
            can_install: false,
            operation_id: None,
            action: None,
            error_code: None,
            review: None,
        }
    }
}

fn install_error_code(error: &InstallPreviewError) -> &'static str {
    match error {
        InstallPreviewError::Cancelled => "cancelled",
        InstallPreviewError::InvalidSelection => "invalid_selection",
        InstallPreviewError::Sdk(_) => "sdk_unavailable",
        InstallPreviewError::PythonVersion | InstallPreviewError::Python(_) => "python_unavailable",
        InstallPreviewError::Plan(_) | InstallPreviewError::InvalidPlan(_) => "invalid_plan",
        InstallPreviewError::ReviewChanged => "review_changed",
        InstallPreviewError::UnsupportedInstall => "unsupported_install",
        InstallPreviewError::WheelPin => "wheel_pin",
        InstallPreviewError::WheelExecution => "wheel_execution",
    }
}

struct InstallState {
    fence: InstallReviewFence,
    gate: InstallApprovalGate,
    visible_preview: Option<(String, String, i32)>,
    preview: Option<JoinHandle<()>>,
    active: Option<(Uuid, JoinHandle<()>)>,
}

pub struct AiHubInstallCoordinator {
    root: PathBuf,
    catalog: SpeechModelCatalog,
    state: AsyncMutex<InstallState>,
    status: watch::Sender<InstallStatus>,
    stop: CancellationToken,
    installation: Arc<InstallationOwner>,
}

impl AiHubInstallCoordinator {
    pub fn new(root: PathBuf) -> Arc<Self> {
        let installation = InstallationOwner::new(root.join("AIData"));
        Self::with_installation_owner(root, installation)
    }

    pub fn with_installation_owner(
        root: PathBuf,
        installation: Arc<InstallationOwner>,
    ) -> Arc<Self> {
        let (status, _) = watch::channel(InstallStatus::default());
        Arc::new(Self {
            catalog: SpeechModelCatalog::for_desktop(root.clone()),
            root,
            state: AsyncMutex::new(InstallState {
                fence: InstallReviewFence::default(),
                gate: InstallApprovalGate::default(),
                visible_preview: None,
                preview: None,
                active: None,
            }),
            status,
            stop: CancellationToken::new(),
            installation,
        })
    }

    pub fn status(&self) -> InstallStatus {
        self.status.borrow().clone()
    }

    pub fn observe_status(&self) -> watch::Receiver<InstallStatus> {
        self.status.subscribe()
    }

    fn publish_status(&self, mut status: InstallStatus) {
        status.revision = self.status.borrow().revision.wrapping_add(1);
        self.status.send_replace(status);
    }

    pub async fn preview(self: &Arc<Self>, category: String, id: String, token: i32) {
        if self.stop.is_cancelled() {
            return;
        }
        let mut state = self.state.lock().await;
        if state
            .active
            .as_ref()
            .is_some_and(|(_, task)| task.is_finished())
            && let Some((_, task)) = state.active.take()
        {
            let _ = task.await;
        }
        state.gate.invalidate();
        state.visible_preview = Some((category.clone(), id.clone(), token));
        self.publish_status(InstallStatus {
            category: category.clone(),
            id: id.clone(),
            token,
            stage: "previewing",
            ..InstallStatus::default()
        });
        let (request, generation) = state.fence.begin();
        if let Some(task) = state.preview.take() {
            task.abort();
            let _ = task.await;
        }
        if self.stop.is_cancelled() {
            return;
        }
        let service = service_for(&category);
        let valid = if let Some(service) = service {
            let snapshot = self.catalog.snapshot().await;
            let matches = match service {
                AiService::Tts => &snapshot.tts,
                AiService::Asr => &snapshot.asr,
                _ => unreachable!(),
            };
            matches.iter().filter(|entry| entry.id == id).count() == 1
        } else {
            false
        };
        if !valid {
            self.publish_status(InstallStatus {
                category: category.clone(),
                id: id.clone(),
                token,
                stage: "failed",
                error_code: Some("invalid_selection"),
                ..InstallStatus::default()
            });
            return;
        }
        let weak = Arc::downgrade(self);
        let root = self.root.clone();
        state.preview = Some(tokio::spawn(async move {
            let result = preview_install_action(&root, service.expect("validated"), &id).await;
            let Some(owner) = weak.upgrade() else {
                return;
            };
            let mut state = owner.state.lock().await;
            if owner.stop.is_cancelled() || !state.fence.accepts(request, generation) {
                return;
            }
            let admitted = match &result {
                Ok(InstallActionReview {
                    review,
                    action: Some(InstallAction::Overlay),
                }) if state.active.is_none() => state.gate.offer(token, review.clone()),
                Ok(InstallActionReview {
                    review,
                    action: Some(InstallAction::Wheel { stage_id, backend }),
                }) if state.active.is_none() => {
                    state
                        .gate
                        .offer_wheel(token, review.clone(), stage_id, backend)
                }
                _ => false,
            };
            let action = result
                .as_ref()
                .ok()
                .and_then(|review| review.action.as_ref());
            let action = action.map(|action| match action {
                InstallAction::Overlay => "overlay",
                InstallAction::Wheel { .. } => "wheel",
            });
            let stage = match (&result, action, admitted) {
                (Err(_), _, _) => "failed",
                (Ok(_), Some("wheel"), true) => "wheel_review",
                (Ok(_), _, true) => "review",
                _ => "not_installable",
            };
            let error_code = result.as_ref().err().map(install_error_code);
            owner.publish_status(InstallStatus {
                category,
                id,
                token,
                stage,
                can_install: admitted,
                action,
                error_code,
                review: result.ok(),
                ..InstallStatus::default()
            });
        }));
    }

    pub async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        state.fence.catalog_replaced();
        state.gate.invalidate();
        state.visible_preview = None;
        let status = self.status();
        self.publish_status(InstallStatus {
            stage: "invalidated",
            category: status.category,
            id: status.id,
            token: status.token,
            ..InstallStatus::default()
        });
        if let Some(task) = state.preview.take() {
            task.abort();
            let _ = task.await;
        }
    }

    pub async fn is_current_preview(&self, category: &str, id: &str, token: i32) -> bool {
        let state = self.state.lock().await;
        !self.stop.is_cancelled()
            && state.visible_preview.as_ref().is_some_and(
                |(current_category, current_id, current_token)| {
                    current_category == category && current_id == id && *current_token == token
                },
            )
    }

    pub async fn confirm(
        self: &Arc<Self>,
        category: String,
        id: String,
        token: i32,
    ) -> Result<(Uuid, InstallAction), &'static str> {
        let mut state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу");
        }
        if state
            .active
            .as_ref()
            .is_some_and(|(_, task)| task.is_finished())
            && let Some((_, task)) = state.active.take()
        {
            let _ = task.await;
        }
        if state.active.is_some() {
            return Err("Установка уже выполняется");
        }
        let service = service_for(&category).ok_or("Неизвестный сервис установки")?;
        let approved = state
            .gate
            .consume_action(token, service, &id)
            .ok_or("План не подтверждён или устарел")?;
        if matches!(approved.action, InstallAction::Overlay)
            && !approved.review.eligible_overlay_install()
        {
            return Err("Выбранная модель или план изменились");
        }
        let snapshot = self.catalog.snapshot().await;
        let entries = match service {
            AiService::Tts => &snapshot.tts,
            AiService::Asr => &snapshot.asr,
            _ => unreachable!(),
        };
        if entries.iter().filter(|entry| entry.id == id).count() != 1 {
            return Err("Модель исчезла из актуального каталога");
        }
        let action = approved.action;
        let component = match &action {
            InstallAction::Overlay => "overlay",
            InstallAction::Wheel { .. } => "wheel",
        };
        let permit = self
            .installation
            .begin(InstallationTarget {
                kind: InstallComponentKind::Package,
                service: Some(service),
                module_id: id.clone(),
                revision: approved.review.identity.plugin_revision.clone(),
                component: component.into(),
            })
            .map_err(|_| "Установка уже выполняется либо журнал недоступен")?;
        let operation = permit.operation();
        let action_name = match &action {
            InstallAction::Overlay => "overlay",
            InstallAction::Wheel { .. } => "wheel",
        };
        self.publish_status(InstallStatus {
            category: category.clone(),
            id: id.clone(),
            token,
            stage: "installing",
            operation_id: Some(operation),
            action: Some(action_name),
            ..InstallStatus::default()
        });
        let action_for_worker = action.clone();
        let root = self.root.clone();
        let owner = Arc::downgrade(self);
        let stop = self.stop.clone();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                _ = permit.cancellation().cancelled() => Err(InstallPreviewError::Cancelled),
                result = async {
                    match &action_for_worker {
                        InstallAction::Overlay => install_reviewed_overlay(&root, &approved.review).await,
                        InstallAction::Wheel { stage_id, backend } => {
                            install_reviewed_wheel_stage(&root, &approved.review, stage_id, backend).await
                        }
                    }
                } => result,
            };
            let result = match result {
                Ok(path) => permit
                    .complete()
                    .map(|_| path)
                    .map_err(|error| InstallPreviewError::Plan(error.to_string())),
                Err(error) => match permit.fail() {
                    Ok(()) => Err(error),
                    Err(journal_error) => Err(InstallPreviewError::Plan(format!(
                        "{error}; installation journal: {journal_error}"
                    ))),
                },
            };
            if !stop.is_cancelled()
                && let Some(owner) = owner.upgrade()
            {
                owner.publish_status(InstallStatus {
                    category: category.clone(),
                    id: id.clone(),
                    token,
                    stage: match &result {
                        Ok(_) => "installed",
                        Err(InstallPreviewError::Cancelled) => "cancelled",
                        Err(_) => "failed",
                    },
                    operation_id: Some(operation),
                    action: Some(action_name),
                    error_code: result.as_ref().err().map(install_error_code),
                    ..InstallStatus::default()
                });
            }
        });
        state.active = Some((operation, task));
        Ok((operation, action))
    }

    pub async fn join_finished(&self, operation: Uuid) -> bool {
        let mut state = self.state.lock().await;
        if state.active.as_ref().is_none_or(|(id, _)| *id != operation) {
            return false;
        }
        if let Some((_, task)) = state.active.take() {
            let _ = task.await;
        }
        true
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        let status = self.status();
        self.publish_status(InstallStatus {
            category: status.category,
            id: status.id,
            token: status.token,
            stage: "closed",
            ..InstallStatus::default()
        });
        let mut state = self.state.lock().await;
        state.fence.catalog_replaced();
        state.gate.invalidate();
        state.visible_preview = None;
        if let Some(task) = state.preview.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some((_, task)) = state.active.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

fn service_for(category: &str) -> Option<AiService> {
    match category {
        "tts" => Some(AiService::Tts),
        "asr" => Some(AiService::Asr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wrong_selection_never_creates_install_authorization() {
        let root = std::env::temp_dir().join(format!("nm-hub-invalid-{}", Uuid::new_v4()));
        let owner = AiHubInstallCoordinator::new(root.clone());
        let mut observer = owner.observe_status();
        owner.preview("unknown".into(), "unknown".into(), 91).await;
        observer.changed().await.unwrap();
        assert_eq!(observer.borrow_and_update().stage, "failed");
        assert_eq!(observer.borrow().error_code, Some("invalid_selection"));
        assert!(
            owner
                .confirm("tts".into(), "unknown".into(), 91)
                .await
                .is_err()
        );
        owner.shutdown().await;
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn status_observer_reports_invalid_preview_and_revoked_gate() {
        let root = std::env::temp_dir().join(format!("nm-hub-status-{}", Uuid::new_v4()));
        let owner = AiHubInstallCoordinator::new(root.clone());
        let mut observer = owner.observe_status();
        assert_eq!(observer.borrow().stage, "idle");
        owner.preview("tts".into(), "missing".into(), 42).await;
        observer.changed().await.unwrap();
        let result = observer.borrow_and_update().clone();
        assert_eq!(result.stage, "failed");
        assert_eq!(result.error_code, Some("invalid_selection"));
        assert_eq!(result.token, 42);
        assert!(!result.can_install);
        assert!(
            owner
                .confirm("tts".into(), "missing".into(), 42)
                .await
                .is_err()
        );
        owner.invalidate().await;
        observer.changed().await.unwrap();
        let revoked = observer.borrow_and_update().clone();
        assert!(revoked.revision > result.revision);
        assert_eq!(revoked.stage, "invalidated");
        assert!(!revoked.can_install);
        owner.shutdown().await;
        assert_eq!(owner.status().stage, "closed");
        owner.preview("tts".into(), "missing".into(), 43).await;
        assert_eq!(owner.status().stage, "closed");
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn shutdown_joins_active_worker_and_cancels_its_future() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Marker(Arc<AtomicBool>);
        impl Drop for Marker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let root = std::env::temp_dir().join(format!("nm-hub-active-{}", Uuid::new_v4()));
        let owner = AiHubInstallCoordinator::new(root.clone());
        let dropped = Arc::new(AtomicBool::new(false));
        let (entered, received) = tokio::sync::oneshot::channel();
        let mark = Marker(Arc::clone(&dropped));
        let task = tokio::spawn(async move {
            let _mark = mark;
            let _ = entered.send(());
            std::future::pending::<()>().await;
        });
        received.await.unwrap();
        owner.state.lock().await.active = Some((Uuid::new_v4(), task));
        owner.shutdown().await;
        assert!(dropped.load(Ordering::SeqCst));
        assert!(owner.state.lock().await.active.is_none());
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn shutdown_closes_admission_without_implicit_restart() {
        let root = std::env::temp_dir().join(format!("nm-hub-stop-{}", Uuid::new_v4()));
        let owner = AiHubInstallCoordinator::new(root.clone());
        owner.shutdown().await;
        owner.preview("tts".into(), "any".into(), 1).await;
        assert!(owner.confirm("tts".into(), "any".into(), 1).await.is_err());
        assert!(!root.exists());
    }
}
