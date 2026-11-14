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
    Speech,
    Releases,
}

#[derive(Clone)]
pub(crate) struct PresenterProbe {
    chat: ChatPresenter,
    runtime: RuntimeStatePresenter,
    speech: super::SpeechSessionPresenter,
    releases: super::ReleasesPresenter,
}

impl PresenterProbe {
    pub(crate) fn new(
        chat: ChatPresenter,
        runtime: RuntimeStatePresenter,
        speech: super::SpeechSessionPresenter,
        releases: super::ReleasesPresenter,
    ) -> Self {
        Self {
            chat,
            runtime,
            speech,
            releases,
        }
    }

    pub(crate) fn request_inspect(&self, request_id: uuid::Uuid) -> Value {
        use crate::request::{ProcessingStage, RequestOutcome};
        let model = self.chat.snapshot();
        let row = model
            .requests
            .iter()
            .find(|row| row.metadata.id.0 == request_id);
        let request = row.map(|row| {
            let (state, stage, outcome, event_id) = match &row.snapshot.state {
                RequestState::Pending => ("pending", None, None, None),
                RequestState::Processing(stage) => {
                    let stage = match stage {
                        ProcessingStage::GeneratingText => "generating_text",
                        ProcessingStage::TextReady { .. } => "text_ready",
                        ProcessingStage::GeneratingVoice { .. } => "generating_voice",
                        ProcessingStage::Accepting { .. } => "accepting",
                    };
                    ("processing", Some(stage), None, None)
                }
                RequestState::Cancelled => ("cancelled", None, None, None),
                RequestState::Finished(result) => {
                    let (outcome, event_id) = match result {
                        RequestOutcome::Success(response) => ("success", Some(response.event_id)),
                        RequestOutcome::VoiceGenerationFailed { response, .. } => {
                            ("voice_error", Some(response.event_id))
                        }
                        RequestOutcome::TextGenerationFailed(_) => ("text_error", None),
                        RequestOutcome::InternalError(_) => ("internal_error", None),
                    };
                    ("finished", None, Some(outcome), event_id)
                }
            };
            json!({
                "request_id": row.metadata.id.0,
                "character": row.metadata.character_id,
                "revision": row.snapshot.revision,
                "state": state,
                "stage": stage,
                "outcome": outcome,
                "event_id": event_id,
                "terminal": row.snapshot.state.is_terminal(),
                "observation_error_present": row.observation_error.is_some(),
            })
        });
        json!({
            "schema_version": 1,
            "source": "chat_presenter_snapshot",
            "request_id": request_id,
            "presenter_revision": model.revision,
            "observed": request.is_some(),
            "request": request,
            "note": "not observed means absent from this bounded presenter view, not absent from Core",
            "evidence_level": "core_ipc_only",
            "full_stack_e2e_verified": false,
        })
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
            PresenterSurface::Speech => {
                if character.is_some() {
                    return Err("speech inspection does not accept character");
                }
                use crate::audio::controller::SpeechSessionPhase;
                let snapshot = self.speech.snapshot();
                let phase = match snapshot.phase {
                    SpeechSessionPhase::Stopped => "stopped",
                    SpeechSessionPhase::Starting => "starting",
                    SpeechSessionPhase::Listening => "listening",
                    SpeechSessionPhase::Stopping => "stopping",
                    SpeechSessionPhase::Failed => "failed",
                };
                Ok(json!({"schema_version":1,"surface":"speech",
                    "source":"speech_session_presenter_snapshot","phase":phase,
                    "has_error":snapshot.last_error.is_some(),"privacy":"no audio or transcript"}))
            }
            PresenterSurface::Releases => {
                if character.is_some() {
                    return Err("releases inspection does not accept character");
                }
                use super::ReleaseLoadState;
                let snapshot = self.releases.snapshot();
                let phase = match snapshot.state {
                    ReleaseLoadState::Idle => "idle",
                    ReleaseLoadState::Loading => "loading",
                    ReleaseLoadState::Ready => "ready",
                    ReleaseLoadState::Failed(_) => "failed",
                };
                Ok(json!({"schema_version":1,"surface":"releases",
                    "source":"releases_presenter_snapshot","phase":phase,
                    "closed":snapshot.closed,"record_count":snapshot.records.len(),
                    "privacy":"no release body or errors"}))
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
