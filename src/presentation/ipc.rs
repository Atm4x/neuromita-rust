//! Sanitized, typed projections of the already-running GUI presenters.
//! No second presenter instance, event stream, or Core mutation is created.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ai_runtime::{AiService, runtime_status::RuntimePhase},
    character::CharacterId,
    request::RequestState,
    server::ServerHandle,
};

use super::{ChatPresenter, RuntimeStatePresenter};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PresenterSurface {
    Chat,
    AiRuntime,
    Connections,
}

#[derive(Clone)]
pub(crate) struct PresenterProbe {
    chat: ChatPresenter,
    runtime: RuntimeStatePresenter,
}

impl PresenterProbe {
    pub(crate) fn new(chat: ChatPresenter, runtime: RuntimeStatePresenter) -> Self {
        Self { chat, runtime }
    }

    pub(crate) fn inspect(
        &self,
        surface: PresenterSurface,
        character: Option<CharacterId>,
        server: &ServerHandle,
    ) -> Result<Value, &'static str> {
        match surface {
            PresenterSurface::Chat => {
                let character = character.ok_or("chat inspection requires a character")?;
                let model = self.chat.snapshot();
                let mut pending = 0;
                let mut processing = 0;
                let mut terminal = 0;
                for row in model
                    .requests
                    .iter()
                    .filter(|r| r.metadata.character_id == character)
                {
                    match &row.snapshot.state {
                        RequestState::Pending => pending += 1,
                        RequestState::Processing(_) => processing += 1,
                        RequestState::Finished(_) | RequestState::Cancelled => terminal += 1,
                    }
                }
                Ok(json!({
                    "schema_version": 1,
                    "surface": "chat",
                    "source": "chat_presenter_snapshot",
                    "revision": model.revision,
                    "closed": model.closed,
                    "character": character,
                    "requests": {"pending":pending,"processing":processing,"terminal":terminal,
                        "total":pending + processing + terminal},
                    "discovery_error_present": model.discovery_error.is_some(),
                    "privacy": "per-character structural counts only, no message bodies or request IDs",
                }))
            }
            PresenterSurface::AiRuntime => {
                if character.is_some() {
                    return Err("ai_runtime does not accept character");
                }
                let snapshot = self.runtime.snapshot();
                let services = [
                    AiService::Tts,
                    AiService::Asr,
                    AiService::Embedding,
                    AiService::Reranker,
                    AiService::Beats,
                ]
                .into_iter()
                .map(|service| {
                    let state = snapshot.service(service);
                    let phase = match state.phase {
                        RuntimePhase::Stopped => "stopped",
                        RuntimePhase::Starting => "starting",
                        RuntimePhase::WorkerRunning => "worker_running",
                        RuntimePhase::ModelReady => "model_ready",
                        RuntimePhase::Failed => "failed",
                    };
                    json!({"service":service,"phase":phase,
                        "generation":state.generation,
                        "model_ready":state.phase == RuntimePhase::ModelReady,
                        "has_error":state.last_error.is_some(),
                        "recovery_exhausted":state.recovery.as_ref().is_some_and(|r| r.exhausted)})
                })
                .collect::<Vec<_>>();
                Ok(json!({"schema_version":1,"surface":"ai_runtime",
                    "source":"runtime_state_presenter_snapshot", "services":services,
                    "privacy":"no model identifiers, secrets or provider error text"}))
            }
            PresenterSurface::Connections => {
                if character.is_some() {
                    return Err("connections does not accept character");
                }
                let state = server.snapshot();
                Ok(json!({"schema_version":1,"surface":"connections",
                    "source":"connections_presenter_server_handle",
                    "running":state.running,"connected_clients":state.connected_clients,
                    "accepted_connections":state.accepted_connections,
                    "has_error":state.last_error.is_some(),
                    "privacy":"no client addresses or error text"}))
            }
        }
    }
}
