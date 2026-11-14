//! Sanitized, typed projections of the already-running GUI presenters.
//! No second presenter instance, event stream, or Core mutation is created.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ai_runtime::{AiService, runtime_status::RuntimePhase},
    character::CharacterId,
    request::{RequestHandle, RequestId, RequestState},
    server::ServerHandle,
    settings::{GuideLevel, Volume},
};

use super::{
    AudioInputPresenter, ChatPresenter, OnboardingSettingsPresenter, ReleasesPresenter,
    RuntimeStatePresenter, VoiceSettingsPresenter,
};

/// Application-level actions are dispatched to the exact presenters used by the GUI.
/// Core-debug calls deliberately use a different command and permission boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PresenterAction {
    ChatReply {
        character: CharacterId,
        text: String,
    },
    ChatReact {
        character: CharacterId,
        event_id: uuid::Uuid,
        #[serde(default)]
        instruction: String,
    },
    ChatInitiate {
        character: CharacterId,
        #[serde(default)]
        instruction: String,
    },
    ChatCancel {
        request_id: uuid::Uuid,
    },
    AudioDevices {},
    AudioStartTest {
        backend: String,
        identity: String,
    },
    AudioStopTest {},
    ReleasesRefresh {},
    RuntimeRefresh {},
    VoiceSetVolume {
        volume: Volume,
    },
    GuideSetLevel {
        level: GuideLevel,
    },
    GuideSetPage {
        page: u8,
    },
    GuideSetCompleted {
        completed: bool,
    },
    PresetSelect {
        preset_id: u32,
    },
    PresetCreateUnconfigured {},
    PresetMove {
        preset_id: u32,
        step: i32,
    },
    PresetDelete {
        preset_id: u32,
    },
    NativeRefresh {},
    NativeSelect {
        model_id: String,
        manifest_sha256: String,
        backend: crate::ai_runtime::native_model::NativeBackend,
    },
    NativeActivate {
        model_id: String,
        service: crate::ai_runtime::native_speech::NativeSpeechKind,
        revision: String,
    },
    NativeStop {
        model_id: String,
        service: crate::ai_runtime::native_speech::NativeSpeechKind,
        revision: String,
    },
    NativeInstall {
        model_id: String,
        manifest_sha256: String,
        backend: crate::ai_runtime::native_model::NativeBackend,
    },
    NativeCancelInstall {},
    AiHubPreviewInstall {
        category: String,
        id: String,
        token: i32,
    },
    AiHubConfirmInstall {
        category: String,
        id: String,
        token: i32,
    },
    PresetUpdateConnection {
        preset_id: u32,
        expected_revision: u64,
        name: String,
        template_id: String,
        endpoint: String,
        model: String,
        temperature: f32,
        max_output_tokens: u32,
        timeout_ms: u64,
        stream: bool,
        max_retries: u8,
        send_temperature: bool,
        send_max_output_tokens: bool,
        #[serde(default)]
        api_key: Option<String>,
    },
}

impl PresenterAction {
    pub(crate) fn generation(&self) -> bool {
        matches!(
            self,
            Self::ChatReply { .. } | Self::ChatReact { .. } | Self::ChatInitiate { .. }
        )
    }
}

pub(crate) enum PresenterActionResult {
    Request(RequestHandle),
    Snapshot(Value),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PresenterSurface {
    Chat,
    AiRuntime,
    Connections,
    Speech,
    Releases,
    AudioInput,
    Voice,
    Guide,
    Presets,
    AiHubInstall,
}

#[derive(Clone)]
pub(crate) struct PresenterProbe {
    pub(super) chat: ChatPresenter,
    runtime: RuntimeStatePresenter,
    speech: super::SpeechSessionPresenter,
    releases: ReleasesPresenter,
    audio_input: AudioInputPresenter,
    voice: VoiceSettingsPresenter,
    guide: OnboardingSettingsPresenter,
    pub(super) conversation: super::ConversationPresenter,
    presets: super::PresetsPresenter,
    preset_selection: super::PresetsSettingsPresenter,
    pub(super) settings: std::sync::Arc<super::SettingsPresenter>,
    pub(super) characters: super::CharactersPresenter,
    pub(super) native:
        Option<std::sync::Arc<crate::ai_runtime::native_service::AiHubNativeService>>,
    pub(super) ai_hub_install:
        Option<std::sync::Arc<crate::ai_runtime::install_coordinator::AiHubInstallCoordinator>>,
}

impl PresenterProbe {
    pub(crate) fn new(
        chat: ChatPresenter,
        runtime: RuntimeStatePresenter,
        speech: super::SpeechSessionPresenter,
        releases: ReleasesPresenter,
        audio_input: AudioInputPresenter,
        voice: VoiceSettingsPresenter,
        guide: OnboardingSettingsPresenter,
        conversation: super::ConversationPresenter,
        presets: super::PresetsPresenter,
        preset_selection: super::PresetsSettingsPresenter,
        settings: std::sync::Arc<super::SettingsPresenter>,
        characters: super::CharactersPresenter,
        native: Option<std::sync::Arc<crate::ai_runtime::native_service::AiHubNativeService>>,
        ai_hub_install: Option<
            std::sync::Arc<crate::ai_runtime::install_coordinator::AiHubInstallCoordinator>,
        >,
    ) -> Self {
        Self {
            chat,
            runtime,
            speech,
            releases,
            audio_input,
            voice,
            guide,
            conversation,
            presets,
            preset_selection,
            settings,
            characters,
            native,
            ai_hub_install,
        }
    }

    pub(crate) async fn conversation_snapshot(
        &self,
        character: CharacterId,
        limit: usize,
        include_content: bool,
    ) -> Result<Value, String> {
        if !(1..=100).contains(&limit) {
            return Err("conversation limit must be between 1 and 100".into());
        }
        let state = self
            .conversation
            .snapshot(character)
            .await
            .map_err(|e| e.to_string())?;
        let total = state.messages.len();
        let messages = state.messages.iter().skip(total.saturating_sub(limit))
            .map(|message| {
                let role = match message.role {
                    crate::timeline::domain::MessageRole::User=>"user",
                    crate::timeline::domain::MessageRole::Assistant=>"assistant",
                    crate::timeline::domain::MessageRole::System=>"system",
                };
                let mut item = json!({"event_id":message.event_id.0,"request_id":message.request_id.map(|id|id.0),
                    "sequence":message.sequence,"role":role,"speaker":message.speaker,
                    "content_length":message.content.chars().count()});
                if include_content {
                    item["content"] = json!(message.content.chars().take(4000).collect::<String>());
                    item["content_truncated"] = json!(message.content.chars().count()>4000);
                }
                item
            }).collect::<Vec<_>>();
        Ok(
            json!({"schema_version":1,"source":"conversation_presenter_snapshot",
            "character":character,"timeline_anchor":state.at,
            "selection":{"branch_id":state.selection.branch_id.0,"generation":state.selection.generation},
            "total_visible_messages":total,"returned":messages.len(),"messages":messages,
            "preset_id":state.preset.0,"preset_name":state.preset_name,
            "content_included":include_content,"evidence_level":"presenter_snapshot",
            "full_stack_e2e_verified":false}),
        )
    }

    pub(crate) async fn execute(
        &self,
        action: PresenterAction,
    ) -> Result<PresenterActionResult, String> {
        use PresenterAction::*;
        let snapshot = match action {
            ChatReply { character, text } => {
                return self
                    .chat
                    .reply(character, text)
                    .map(PresenterActionResult::Request)
                    .map_err(|e| e.to_string());
            }
            ChatReact {
                character,
                event_id,
                instruction,
            } => {
                return self
                    .chat
                    .react(character, crate::identity::EventId(event_id), instruction)
                    .map(PresenterActionResult::Request)
                    .map_err(|e| e.to_string());
            }
            ChatInitiate {
                character,
                instruction,
            } => {
                return self
                    .chat
                    .initiate(character, instruction)
                    .map(PresenterActionResult::Request)
                    .map_err(|e| e.to_string());
            }
            ChatCancel { request_id } => {
                let result = self
                    .chat
                    .cancel(RequestId(request_id))
                    .map_err(|e| e.to_string())?;
                json!({"action":"chat_cancel","request_id":request_id,"result":format!("{result:?}")})
            }
            AudioDevices {} => {
                let devices = self
                    .audio_input
                    .devices()
                    .await
                    .map_err(|e| e.to_string())?;
                json!({"action":"audio_devices","devices":devices.into_iter().take(64)
                    .map(|d| json!({"id":d.id,"name":d.name,"backend":d.backend})).collect::<Vec<_>>()})
            }
            AudioStartTest { backend, identity } => {
                if backend.len() > 64
                    || identity.len() > 512
                    || backend.is_empty()
                    || identity.is_empty()
                {
                    return Err("invalid audio backend or device identity".into());
                }
                self.audio_input
                    .start_test(&backend, &identity)
                    .await
                    .map_err(|e| e.to_string())?;
                json!({"action":"audio_start_test","started":true})
            }
            AudioStopTest {} => {
                self.audio_input
                    .stop_test()
                    .await
                    .map_err(|e| e.to_string())?;
                json!({"action":"audio_stop_test","stopped":true})
            }
            ReleasesRefresh {} => {
                self.releases.refresh().map_err(|e| e.to_string())?;
                json!({"action":"releases_refresh","queued":true})
            }
            RuntimeRefresh {} => {
                self.runtime.refresh_status().await;
                json!({"action":"runtime_refresh","refreshed":true})
            }
            VoiceSetVolume { volume } => {
                self.voice.volume.set(volume).map_err(|e| e.to_string())?;
                json!({"action":"voice_set_volume","value":volume.get()})
            }
            GuideSetLevel { level } => {
                self.guide
                    .guide_level
                    .set(level)
                    .map_err(|e| e.to_string())?;
                json!({"action":"guide_set_level","level":level})
            }
            GuideSetPage { page } => {
                self.guide.guide_page.set(page).map_err(|e| e.to_string())?;
                json!({"action":"guide_set_page","page":page})
            }
            GuideSetCompleted { completed } => {
                self.guide
                    .guide_completed
                    .set(completed)
                    .map_err(|e| e.to_string())?;
                json!({"action":"guide_set_completed","completed":completed})
            }
            PresetSelect { preset_id } => {
                let id = crate::generation::PresetId(preset_id);
                self.presets.require(id).map_err(|e| e.to_string())?;
                self.preset_selection
                    .active
                    .set(id)
                    .map_err(|e| e.to_string())?;
                json!({"action":"preset_select","preset_id":preset_id})
            }
            PresetCreateUnconfigured {} => {
                let (id, record) = self
                    .presets
                    .create_unconfigured()
                    .map_err(|e| e.to_string())?;
                json!({"action":"preset_create_unconfigured","preset_id":id.0,"name":record.name})
            }
            PresetMove { preset_id, step } => {
                if !matches!(step, -1 | 1) {
                    return Err("preset move must be -1 or +1".into());
                }
                self.presets
                    .move_one(crate::generation::PresetId(preset_id), step)
                    .map_err(|e| e.to_string())?;
                json!({"action":"preset_move","preset_id":preset_id,"step":step})
            }
            PresetDelete { preset_id } => {
                self.presets
                    .delete(crate::generation::PresetId(preset_id))
                    .map_err(|e| e.to_string())?;
                json!({"action":"preset_delete","preset_id":preset_id})
            }
            NativeRefresh {} => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                let snapshot = native.refresh().await?;
                json!({"action":"native_refresh","revision_count":snapshot.inventory.revisions.len(),
                    "selection_count":snapshot.inventory.selections.len()})
            }
            NativeSelect {
                model_id,
                manifest_sha256,
                backend,
            } => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                native.select(&model_id, &manifest_sha256, backend).await?;
                json!({"action":"native_select","model_id":model_id,"backend":backend.as_str(),
                    "manifest_sha256":manifest_sha256})
            }
            NativeActivate {
                model_id,
                service,
                revision,
            } => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                let worker = native
                    .activate_exact_for_service(service, &model_id, &revision)
                    .await?;
                json!({"action":"native_activate","model_id":worker.model_id,
                    "session_id":worker.session_id,"phase":format!("{:?}",worker.phase),
                    "service":service})
            }
            NativeStop {
                model_id,
                service,
                revision,
            } => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                native
                    .deactivate_exact_for_service(service, &model_id, &revision)
                    .await?;
                json!({"action":"native_stop","model_id":model_id,"service":service})
            }
            NativeInstall {
                model_id,
                manifest_sha256,
                backend,
            } => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
                let operation_id = native
                    .start_download(
                        crate::ai_runtime::native_download_operation::NativeDownloadSelection {
                            model_id: model_id.clone(),
                            manifest_sha256: manifest_sha256.clone(),
                            backend,
                        },
                        sender,
                    )
                    .await?;
                let owner = std::sync::Arc::clone(native);
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        if let crate::ai_runtime::native_download_operation::NativeDownloadEvent::Finished { id, .. } = event {
                            let _ = owner.finish_download(id).await;
                            break;
                        }
                    }
                });
                json!({"action":"native_install","operation_id":operation_id,
                    "model_id":model_id,"manifest_sha256":manifest_sha256,"backend":backend.as_str(),
                    "accepted":true})
            }
            NativeCancelInstall {} => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                let requested = native.cancel_download().await;
                json!({"action":"native_cancel_install","cancel_requested":requested})
            }
            AiHubPreviewInstall {
                category,
                id,
                token,
            } => {
                if !matches!(category.as_str(), "tts" | "asr")
                    || id.is_empty()
                    || id.len() > 256
                    || !id.is_ascii()
                {
                    return Err("invalid AI Hub installation category or ID".into());
                }
                let owner = self
                    .ai_hub_install
                    .as_ref()
                    .ok_or("AI Hub install coordinator is unavailable")?;
                owner.preview(category, id, token).await;
                json!({"action":"ai_hub_preview_install", "status":owner.status()})
            }
            AiHubConfirmInstall {
                category,
                id,
                token,
            } => {
                if !matches!(category.as_str(), "tts" | "asr")
                    || id.is_empty()
                    || id.len() > 256
                    || !id.is_ascii()
                {
                    return Err("invalid AI Hub installation category or ID".into());
                }
                let owner = self
                    .ai_hub_install
                    .as_ref()
                    .ok_or("AI Hub install coordinator is unavailable")?;
                let (operation, action) = owner
                    .confirm(category, id, token)
                    .await
                    .map_err(str::to_owned)?;
                json!({"action":"ai_hub_confirm_install", "operation_id":operation,
                    "install_action":match action {
                        crate::ai_runtime::install_admission::InstallAction::Overlay=>"overlay",
                        crate::ai_runtime::install_admission::InstallAction::Wheel { .. }=>"wheel",
                    },"status":owner.status()})
            }
            PresetUpdateConnection {
                preset_id,
                expected_revision,
                name,
                template_id,
                endpoint,
                model,
                temperature,
                max_output_tokens,
                timeout_ms,
                stream,
                max_retries,
                send_temperature,
                send_max_output_tokens,
                api_key,
            } => {
                use crate::generation::{
                    PresetConnectionEdit, PresetDefinition, PresetGenerationEdit, PresetId,
                    PresetProtocol, SecretKey,
                };
                if name.trim().is_empty()
                    || name.len() > 256
                    || endpoint.len() > 2048
                    || model.len() > 256
                    || template_id.len() > 64
                    || max_retries > 4
                    || api_key
                        .as_ref()
                        .is_some_and(|key| key.len() > 16384 || key.is_empty())
                {
                    return Err("invalid API preset connection parameters".into());
                }
                let connection = if template_id.is_empty() {
                    if api_key.is_some() {
                        return Err("unconfigured preset cannot accept API key".into());
                    }
                    None
                } else {
                    let template = super::PresetsPresenter::template(&template_id)
                        .map_err(|e| e.to_string())?;
                    let existing_credential = self
                        .presets
                        .list()
                        .into_iter()
                        .find(|(id, _)| id.0 == preset_id)
                        .and_then(|(_, record)| match record.definition {
                            PresetDefinition::Gemini(config)
                                if template.protocol == PresetProtocol::Gemini =>
                            {
                                Some(config.credential)
                            }
                            PresetDefinition::OpenAi(config)
                                if template.protocol == PresetProtocol::OpenAi(config.protocol) =>
                            {
                                config.credential
                            }
                            _ => None,
                        });
                    Some(PresetGenerationEdit {
                        protocol: template.protocol,
                        send_temperature,
                        send_max_output_tokens,
                        stream,
                        max_retries,
                        endpoint,
                        model,
                        credential: existing_credential.or(Some(template.credential)),
                        temperature,
                        max_output_tokens,
                        timeout_ms,
                    })
                };
                self.presets
                    .update_connection_checked(
                        PresetId(preset_id),
                        expected_revision,
                        PresetConnectionEdit {
                            name,
                            connection,
                            key: api_key.map(SecretKey::new),
                        },
                    )
                    .map_err(|e| e.to_string())?;
                let revision = self.presets.snapshot().revision;
                json!({"action":"preset_update_connection","preset_id":preset_id,
                    "revision":revision,"credential_secret_returned":false})
            }
        };
        Ok(PresenterActionResult::Snapshot(json!({
            "source":"gui_presenter", "evidence_level":"presenter_command",
            "full_stack_e2e_verified":false,"result":snapshot
        })))
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
            PresenterSurface::AudioInput => {
                if character.is_some() {
                    return Err("audio_input does not accept character");
                }
                let monitor = self.audio_input.observe_test();
                let status = monitor.borrow().clone();
                Ok(
                    json!({"schema_version":1,"surface":"audio_input", "source":"audio_input_presenter",
                    "running":status.running,"level":status.level,
                    "device":status.device,"has_error":status.error.is_some(),
                    "privacy":"no audio samples or error text"}),
                )
            }
            PresenterSurface::AiHubInstall => {
                if character.is_some() {
                    return Err("ai_hub_install does not accept character");
                }
                let owner = self
                    .ai_hub_install
                    .as_ref()
                    .ok_or("AI Hub installation is not configured")?;
                Ok(json!({"schema_version":1,"surface":"ai_hub_install",
                    "source":"ai_hub_install_coordinator", "status":owner.status(),
                    "privacy":"no installer plan, environment paths or credentials"}))
            }
            PresenterSurface::Presets => {
                if character.is_some() {
                    return Err("presets does not accept character");
                }
                let view = self.presets.snapshot();
                let revision = view.revision;
                let records = view.records.iter().take(256)
                    .map(|record| json!({"id":record.id.0,"name":record.name,
                        "configured":record.configured,"template_id":record.template_id,
                        "model":record.model,"endpoint":record.endpoint,
                        "temperature":record.temperature,"max_output_tokens":record.max_output_tokens,
                        "timeout_ms":record.timeout_ms,"stream":record.stream,
                        "max_retries":record.max_retries,"send_temperature":record.send_temperature,
                        "send_max_output_tokens":record.send_max_output_tokens,
                        "credential_present":record.api_key.as_ref().is_some_and(|key| !key.expose().is_empty()),
                        "credential_error_present":record.credential_error.is_some()}))
                    .collect::<Vec<_>>();
                Ok(
                    json!({"schema_version":1,"surface":"presets","source":"presets_presenter",
                    "active_preset_id":self.preset_selection.active.get().0,"revision":revision,"records":records,
                    "privacy":"no provider credentials or prompt templates"}),
                )
            }
            PresenterSurface::Voice => {
                if character.is_some() {
                    return Err("voice does not accept character");
                }
                Ok(
                    json!({"schema_version":1,"surface":"voice","source":"voice_settings_presenter",
                    "volume":self.voice.volume.get().get(),
                    "in_game":self.voice.in_game.get(),"in_launcher":self.voice.in_launcher.get(),
                    "privacy":"voice configuration, no provider secrets"}),
                )
            }
            PresenterSurface::Guide => {
                if character.is_some() {
                    return Err("guide does not accept character");
                }
                Ok(
                    json!({"schema_version":1,"surface":"guide","source":"onboarding_settings_presenter",
                    "level":self.guide.guide_level.get(),"page":self.guide.guide_page.get(),
                    "completed":self.guide.guide_completed.get()}),
                )
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
