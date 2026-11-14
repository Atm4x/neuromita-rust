//! Core-owned admission and supervised execution for AI Hub installation.
//! The desktop renders events but never owns an approved plan or an installer task.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::{sync::Mutex as AsyncMutex, sync::mpsc, task::JoinHandle};
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

pub enum InstallEvent {
    Preview {
        category: String,
        id: String,
        token: i32,
        result: Result<InstallActionReview, InstallPreviewError>,
        admitted: bool,
    },
    Finished {
        operation: Uuid,
        category: String,
        id: String,
        token: i32,
        action: InstallAction,
        result: Result<PathBuf, InstallPreviewError>,
    },
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
    events: mpsc::UnboundedSender<InstallEvent>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<InstallEvent>>>,
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
        let (events, receiver) = mpsc::unbounded_channel();
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
            events,
            receiver: Mutex::new(Some(receiver)),
            stop: CancellationToken::new(),
            installation,
        })
    }

    pub fn take_events(&self) -> Option<mpsc::UnboundedReceiver<InstallEvent>> {
        self.receiver.lock().ok()?.take()
    }

    pub async fn preview(self: &Arc<Self>, category: String, id: String, token: i32) {
        let mut state = self.state.lock().await;
        state.gate.invalidate();
        state.visible_preview = Some((category.clone(), id.clone(), token));
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
            let _ = self.events.send(InstallEvent::Preview {
                category,
                id,
                token,
                result: Err(InstallPreviewError::InvalidSelection),
                admitted: false,
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
            let _ = owner.events.send(InstallEvent::Preview {
                category,
                id,
                token,
                result,
                admitted,
            });
        }));
    }

    pub async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        state.fence.catalog_replaced();
        state.gate.invalidate();
        state.visible_preview = None;
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
        let action_for_worker = action.clone();
        let root = self.root.clone();
        let events = self.events.clone();
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
            if !stop.is_cancelled() {
                let _ = events.send(InstallEvent::Finished {
                    operation,
                    category,
                    id,
                    token,
                    action: action_for_worker,
                    result,
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
        let mut events = owner.take_events().unwrap();
        owner.preview("unknown".into(), "unknown".into(), 91).await;
        assert!(matches!(
            events.recv().await,
            Some(InstallEvent::Preview {
                result: Err(InstallPreviewError::InvalidSelection),
                admitted: false,
                ..
            })
        ));
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
