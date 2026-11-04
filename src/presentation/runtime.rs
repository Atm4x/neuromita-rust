//! Read-only presentation of the application's AI worker lifecycle.

use std::sync::Weak;

use tokio::sync::watch;

use crate::ai_runtime::{AiSupervisor, runtime_status::RuntimeSnapshot};

/// Read-only presenter-facing view of the supervisor's latest state.
/// Subscribers receive coalesced snapshots rather than an event history.
pub struct RuntimeStatePresenter {
    updates: Option<watch::Receiver<RuntimeSnapshot>>,
    supervisor: Weak<AiSupervisor>,
}

impl RuntimeStatePresenter {
    pub(crate) fn new(updates: Option<watch::Receiver<RuntimeSnapshot>>) -> Self {
        Self {
            updates,
            supervisor: Weak::new(),
        }
    }

    pub(crate) fn with_supervisor(mut self, supervisor: Weak<AiSupervisor>) -> Self {
        self.supervisor = supervisor;
        self
    }

    /// Reconcile a worker that exited outside any IPC operation.
    /// No new process or model is created by this call.
    pub async fn refresh_status(&self) -> RuntimeSnapshot {
        if let Some(supervisor) = self.supervisor.upgrade() {
            supervisor.refresh_status().await
        } else {
            self.snapshot()
        }
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.updates
            .as_ref()
            .map_or_else(RuntimeSnapshot::default, |updates| updates.borrow().clone())
    }

    pub fn observe(&self) -> Option<watch::Receiver<RuntimeSnapshot>> {
        self.updates.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::{
        AiService,
        runtime_status::{RuntimePhase, ServiceRuntimeStatus},
    };

    #[test]
    fn detached_presenter_never_reports_an_active_model() {
        let presenter = RuntimeStatePresenter::new(None);
        let snapshot = presenter.snapshot();
        for service in [AiService::Tts, AiService::Asr] {
            let status = snapshot.service(service);
            assert_eq!(status.phase, RuntimePhase::Stopped);
            assert!(status.generation.is_none());
            assert!(status.active_model.is_none());
        }
        assert!(presenter.observe().is_none());
    }

    #[tokio::test]
    async fn detached_refresh_is_inert_and_never_starts_a_worker() {
        let presenter = RuntimeStatePresenter::new(None);
        let state = presenter.refresh_status().await;
        for service in [AiService::Tts, AiService::Asr] {
            assert_eq!(state.service(service).phase, RuntimePhase::Stopped);
        }
    }

    #[tokio::test]
    async fn presenter_observes_typed_snapshots_without_editing_them() {
        let (sender, updates) = watch::channel(RuntimeSnapshot::default());
        let presenter = RuntimeStatePresenter::new(Some(updates));
        let mut observed = presenter.observe().unwrap();
        sender.send_modify(|snapshot| {
            snapshot.services.insert(
                AiService::Asr,
                ServiceRuntimeStatus {
                    phase: RuntimePhase::Starting,
                    pending_model: Some("whisper".into()),
                    ..Default::default()
                },
            );
        });
        observed.changed().await.unwrap();
        let state = presenter.snapshot().service(AiService::Asr);
        assert_eq!(state.phase, RuntimePhase::Starting);
        assert_eq!(state.pending_model.as_deref(), Some("whisper"));
        assert!(state.active_model.is_none());
    }
}
