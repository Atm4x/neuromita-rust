use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    character::CharacterRegistry, games::GamePolicy, generation::PresetCatalog,
    request::ChatApiState, settings::SettingsOwner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
}

pub(crate) struct ApiBindings {
    pub(crate) chat: Arc<ChatApiState>,
    pub(crate) timeline: Arc<Mutex<crate::timeline::SessionTimeline>>,
    pub(crate) characters: CharacterRegistry,
    pub(crate) knowledge: Arc<crate::knowledge::KnowledgeRetrieval>,
    pub(crate) graph_extractor: Option<Arc<dyn crate::knowledge::graph::GraphExtractor>>,
    pub(crate) settings: Arc<SettingsOwner>,
    pub(crate) presets: Arc<PresetCatalog>,
    pub(crate) games: Arc<GamePolicy>,
    pub(crate) reminders: Arc<crate::reminders::RemindersHandle>,
    pub(crate) presenter_probe: OnceLock<PresenterProbe>,
}


#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PresenterSurface {
    Chat,
    AiRuntime,
    Connections,
}

pub(crate) struct PresenterProbe {
    chat: crate::presentation::ChatPresenter,
    ai_runtime: crate::presentation::RuntimeStatePresenter,
    connections: crate::presentation::ConnectionsPresenter,
}

impl PresenterProbe {
    pub(crate) fn new(
        chat: crate::presentation::ChatPresenter,
        ai_runtime: crate::presentation::RuntimeStatePresenter,
        connections: crate::presentation::ConnectionsPresenter,
    ) -> Self {
        Self { chat, ai_runtime, connections }
    }

    pub(crate) fn snapshot(&self, surface: PresenterSurface) -> serde_json::Value {
        use serde_json::json;
        match surface {
            PresenterSurface::Chat => {
                let model = self.chat.snapshot();
                let requests = model.requests.iter().rev().take(32).map(|row| {
                    let (state, outcome) = match &row.snapshot.state {
                        crate::request::RequestState::Pending => ("pending", None),
                        crate::request::RequestState::Processing(_) => ("processing", None),
                        crate::request::RequestState::Cancelled => ("cancelled", None),
                        crate::request::RequestState::Finished(outcome) => {
                            let outcome = match outcome {
                                crate::request::RequestOutcome::Success(_) => "success",
                                crate::request::RequestOutcome::VoiceGenerationFailed { .. } => "voice_error",
                                crate::request::RequestOutcome::TextGenerationFailed(_) => "text_error",
                                crate::request::RequestOutcome::InternalError(_) => "internal_error",
                            };
                            ("finished", Some(outcome))
                        }
                    };
                    json!({
                        "request_id": row.metadata.id.0,
                        "character": row.metadata.character_id,
                        "revision": row.snapshot.revision,
                        "state": state,
                        "outcome": outcome,
                        "observation_error_present": row.observation_error.is_some()
                    })
                }).collect::<Vec<_>>();
                json!({
                    "schema_version": 1,
                    "source": "gui_presenter_watch",
                    "surface": "chat",
                    "revision": model.revision,
                    "closed": model.closed,
                    "tracked_count": model.requests.len(),
                    "recent": requests,
                    "discovery_error_present": model.discovery_error.is_some(),
                    "evidence_level": "core_ipc_only",
                    "full_stack_e2e_verified": false
                })
            }
            PresenterSurface::AiRuntime => {
                use crate::ai_runtime::{AiService, runtime_status::RuntimePhase};
                let snapshot = self.ai_runtime.snapshot();
                let services = [
                    AiService::Tts, AiService::Asr, AiService::Embedding,
                    AiService::Reranker, AiService::Beats,
                ].into_iter().map(|service| {
                    let state = snapshot.service(service);
                    let phase = match state.phase {
                        RuntimePhase::Stopped => "stopped",
                        RuntimePhase::Starting => "starting",
                        RuntimePhase::WorkerRunning => "worker_running",
                        RuntimePhase::ModelReady => "model_ready",
                        RuntimePhase::Failed => "failed",
                    };
                    json!({
                        "service": service,
                        "phase": phase,
                        "generation": state.generation,
                        "active_model_present": state.active_model.is_some(),
                        "pending_model_present": state.pending_model.is_some(),
                        "last_error_present": state.last_error.is_some(),
                        "recovery_attempts": state.recovery.as_ref().map(|v| v.attempted),
                        "recovery_exhausted": state.recovery.as_ref().map(|v| v.exhausted)
                    })
                }).collect::<Vec<_>>();
                json!({
                    "schema_version": 1,
                    "source": "gui_presenter_watch",
                    "surface": "ai_runtime",
                    "services": services,
                    "evidence_level": "core_ipc_only",
                    "full_stack_e2e_verified": false
                })
            }
            PresenterSurface::Connections => {
                let state = self.connections.snapshot();
                json!({
                    "schema_version": 1,
                    "source": "gui_presenter_watch",
                    "surface": "connections",
                    "running": state.running,
                    "connected_clients": state.connected_clients,
                    "accepted_connections": state.accepted_connections,
                    "last_error_present": state.last_error.is_some(),
                    "evidence_level": "core_ipc_only",
                    "full_stack_e2e_verified": false
                })
            }
        }
    }
}

struct Slot {
    status: ApplicationStatus,
    owner: Option<Uuid>,
    bindings: Option<Arc<ApiBindings>>,
    initialized: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    status: ApplicationStatus::Stopped,
    owner: None,
    bindings: None,
    initialized: false,
});
static CHANGED: Notify = Notify::const_new();

pub(crate) enum ApiUnavailable {
    Uninitialized,
    Stopped,
}

pub(crate) fn current() -> Result<Arc<ApiBindings>, ApiUnavailable> {
    let slot = SLOT.lock().expect("application slot poisoned");
    if let Some(bindings) = &slot.bindings {
        return Ok(Arc::clone(bindings));
    }
    Err(
        if slot.initialized || slot.status != ApplicationStatus::Stopped {
            ApiUnavailable::Stopped
        } else {
            ApiUnavailable::Uninitialized
        },
    )
}

pub(crate) fn status() -> ApplicationStatus {
    SLOT.lock().expect("application slot poisoned").status
}

pub(crate) async fn wait_stopped() {
    loop {
        let changed = CHANGED.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if status() == ApplicationStatus::Stopped {
            return;
        }
        changed.await;
    }
}

pub(crate) struct RuntimeLease {
    id: Uuid,
}

impl RuntimeLease {
    pub(crate) fn reserve() -> Option<Self> {
        let mut slot = SLOT.lock().expect("application slot poisoned");
        if slot.owner.is_some() {
            return None;
        }
        let id = Uuid::new_v4();
        slot.owner = Some(id);
        slot.status = ApplicationStatus::Starting;
        CHANGED.notify_waiters();
        Some(Self { id })
    }

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn publish(&self, bindings: Arc<ApiBindings>) {
        let mut slot = SLOT.lock().expect("application slot poisoned");
        assert_eq!(slot.owner, Some(self.id));
        assert_eq!(slot.status, ApplicationStatus::Starting);
        slot.bindings = Some(bindings);
        slot.initialized = true;
        slot.status = ApplicationStatus::Running;
        CHANGED.notify_waiters();
    }
}

pub(crate) fn begin_stopping(id: Uuid) {
    let mut slot = SLOT.lock().expect("application slot poisoned");
    if slot.owner == Some(id) {
        slot.bindings = None;
        slot.status = ApplicationStatus::Stopping;
        CHANGED.notify_waiters();
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let mut slot = SLOT.lock().expect("application slot poisoned");
        if slot.owner == Some(self.id) {
            slot.bindings = None;
            slot.owner = None;
            slot.status = ApplicationStatus::Stopped;
            CHANGED.notify_waiters();
        }
    }
}
