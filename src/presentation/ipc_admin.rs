//! Admin MCP operations use precisely the same Presenter handles and owners as the GUI.
//! No private data is returned unless specifically requested by the caller.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{ConversationChange, MemoryListPage, MemoryListQuery, PresenterProbe};
use crate::{
    character::{
        CharacterId,
        memory::{MemoryId, MemoryPriority, MemoryState},
    },
    identity::EventId,
    memory::MemoryCommand,
    request::{RequestId, TimelineFence},
    settings::{CharacterSettings, Language, PresetSelection, StructuredDisplayMode},
    timeline::selection::TimelineSelection,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "query", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PresenterRead {
    SpeechInventory {},
    ChatDiagnostics {
        character: CharacterId,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    ConversationRefresh {
        character: CharacterId,
    },
    MemoryRecall {
        character: CharacterId,
        memory_id: MemoryId,
        query_text: String,
    },
    GraphPreview {
        character: CharacterId,
        memory_id: MemoryId,
        mode: GraphReviewMode,
    },
    PromptFolder {
        character: CharacterId,
    },
    PromptCatalog {
        #[serde(default = "default_prompt_catalog_limit")]
        limit: usize,
    },
    CharacterDebug {
        character: CharacterId,
        #[serde(default)]
        include_content: bool,
    },
    SpeechTranscript {
        #[serde(default)]
        include_content: bool,
    },
    NativeModels {
        #[serde(default)]
        refresh: bool,
    },
    Settings {
        area: SettingsArea,
        #[serde(default)]
        character: Option<CharacterId>,
    },
    CharacterProfile {
        character: CharacterId,
        #[serde(default)]
        include_content: bool,
    },
    Memories {
        character: CharacterId,
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_limit")]
        limit: usize,
        #[serde(default)]
        include_content: bool,
        #[serde(default)]
        state: Option<MemoryState>,
    },
    History {
        character: CharacterId,
        #[serde(default)]
        offset: usize,
        #[serde(default = "default_limit")]
        limit: usize,
        #[serde(default)]
        include_content: bool,
        #[serde(default)]
        version: Option<ConversationVersion>,
    },
    ConversationPreview {
        character: CharacterId,
        change: ConversationEdit,
    },
    Wait {
        target: WaitTarget,
        #[serde(default)]
        character: Option<CharacterId>,
        #[serde(default)]
        after_revision: Option<u64>,
        #[serde(default)]
        after_version: Option<String>,
        #[serde(default = "default_wait_ms")]
        timeout_ms: u64,
    },
    RegeneratePreview {
        character: CharacterId,
        event_id: Uuid,
        version: ConversationVersion,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphReviewMode {
    Clear,
    Reindex,
}

fn mode_name(mode: crate::knowledge::maintenance::GraphMaintenanceMode) -> &'static str {
    match mode {
        crate::knowledge::maintenance::GraphMaintenanceMode::Clear => "clear",
        crate::knowledge::maintenance::GraphMaintenanceMode::Reindex => "reindex",
    }
}

fn default_prompt_catalog_limit() -> usize {
    100
}

fn default_wait_ms() -> u64 {
    10000
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitTarget {
    Settings,
    Chat,
    Memory,
    NativeDownload,
    AiHubInstall,
    SpeechTranscript,
    TelegramAuth,
    RemoteVoice,
}

fn default_limit() -> usize {
    40
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingsArea {
    Interface,
    Voice,
    Memory,
    Capture,
    Onboarding,
    Tts,
    Asr,
    HistoryGeneration,
    DocumentGeneration,
    Tools,
    Persistence,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationVersion {
    pub branch_id: Uuid,
    pub generation: u64,
    pub through: u64,
}
impl ConversationVersion {
    fn matches(&self, fence: TimelineFence) -> bool {
        self.branch_id == fence.selection.branch_id.0
            && self.generation == fence.selection.generation
            && self.through == fence.at.through
            && self.branch_id == fence.at.branch_id
    }
    fn fence(self) -> TimelineFence {
        TimelineFence {
            at: crate::timeline::ContextAnchor {
                branch_id: self.branch_id,
                through: self.through,
            },
            selection: TimelineSelection {
                branch_id: crate::timeline::domain::BranchId(self.branch_id),
                generation: self.generation,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ConversationEdit {
    ClearMessages,
    ResetContext,
    HideMessage { event_id: Uuid },
    ReviseMessage { event_id: Uuid, content: String },
}
impl ConversationEdit {
    fn into_change(self) -> ConversationChange {
        match self {
            Self::ClearMessages => ConversationChange::ClearMessages,
            Self::ResetContext => ConversationChange::ResetContext,
            Self::HideMessage { event_id } => ConversationChange::HideMessage(EventId(event_id)),
            Self::ReviseMessage { event_id, content } => {
                ConversationChange::ReviseMessage(EventId(event_id), content)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "change", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PresenterWrite {
    Settings {
        field: SettingsField,
        value: Value,
    },
    CharacterProfile {
        character: CharacterId,
        expected_revision: u64,
        profile: CharacterSettings,
    },
    CharacterPresetAssignment {
        character: CharacterId,
        expected_revision: u64,
        slot: CharacterPresetSlot,
        selection: PresetSelection,
    },
    CharacterPromptSelection {
        character: CharacterId,
        expected_revision: u64,
        prompt_set: Option<String>,
    },
    Memory {
        character: CharacterId,
        expected_version: String,
        commands: Vec<AdminMemoryCommand>,
    },
    Conversation {
        character: CharacterId,
        version: ConversationVersion,
        edit: ConversationEdit,
    },
    RetryFailed {
        character: CharacterId,
        request_id: Uuid,
    },
    GraphApply {
        character: CharacterId,
        request_id: Uuid,
    },
    GraphAbort {
        request_id: Uuid,
    },
    GuideRetry {},
    SettingsRetry {},
    PromptFolderOpen {
        character: CharacterId,
    },
    AttachFile {
        character: CharacterId,
        version: ConversationVersion,
        text: String,
        source: String,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CharacterPresetSlot {
    Reply,
    React,
    Initiate,
    All,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SettingsField {
    Language,
    FontSize,
    HidePrivate,
    StructuredDisplay,
    VoiceVolume,
    VoiceInLauncher,
    VoiceInGame,
    VoiceName,
    MemoryDedupEnabled,
    MemoryDedupThreshold,
    SectionVisibility,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AdminMemoryCommand {
    Add {
        priority: MemoryPriority,
        content: String,
    },
    EnsureFact {
        priority: MemoryPriority,
        content: String,
    },
    Update {
        id: MemoryId,
        #[serde(default)]
        priority: Option<MemoryPriority>,
        content: String,
    },
    Forget {
        id: MemoryId,
    },
    Restore {
        id: MemoryId,
    },
    SoftDelete {
        id: MemoryId,
    },
    SetProtection {
        id: MemoryId,
        protected: bool,
    },
    SetExpiry {
        id: MemoryId,
        expires_at_unix_ms: Option<u64>,
    },
}
impl From<AdminMemoryCommand> for MemoryCommand {
    fn from(value: AdminMemoryCommand) -> Self {
        match value {
            AdminMemoryCommand::Add { priority, content } => Self::Add { priority, content },
            AdminMemoryCommand::EnsureFact { priority, content } => {
                Self::EnsureFact { priority, content }
            }
            AdminMemoryCommand::Update {
                id,
                priority,
                content,
            } => Self::Update {
                id,
                priority,
                content,
            },
            AdminMemoryCommand::Forget { id } => Self::Forget { id },
            AdminMemoryCommand::Restore { id } => Self::Restore { id },
            AdminMemoryCommand::SoftDelete { id } => Self::SoftDelete { id },
            AdminMemoryCommand::SetProtection { id, protected } => {
                Self::SetProtection { id, protected }
            }
            AdminMemoryCommand::SetExpiry {
                id,
                expires_at_unix_ms,
            } => Self::SetExpiry {
                id,
                expires_at_unix_ms,
            },
        }
    }
}

fn memory_version(snapshot: &crate::character::CharacterMemorySnapshot) -> String {
    use ring::digest::{SHA256, digest};
    let data = format!("{snapshot:?}");
    digest(&SHA256, data.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn messages_json(
    messages: &[crate::timeline::domain::ProjectedMessage],
    include_content: bool,
) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            let mut entry = json!({"event_id":m.event_id.0,"request_id":m.request_id.map(|id|id.0),
            "sequence":m.sequence,"role":format!("{:?}",m.role).to_lowercase(),"speaker":m.speaker,
            "content_length":m.content.chars().count()});
            if include_content {
                entry["content"] = json!(m.content.chars().take(4000).collect::<String>());
                entry["truncated"] = json!(m.content.chars().count() > 4000);
            }
            entry
        })
        .collect()
}

impl PresenterProbe {
    fn validate_character_profile(
        &self,
        character: CharacterId,
        profile: &CharacterSettings,
    ) -> Result<(), String> {
        if let Some(name) = profile.prompt_set.as_deref() {
            self.prompt_folder.validate_selection(character, name)?;
        }
        for selection in [
            profile.presets.reply,
            profile.presets.react,
            profile.presets.initiate,
        ] {
            if let PresetSelection::Specific(id) = selection {
                self.presets
                    .require(id)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub(crate) async fn admin_read(&self, query: PresenterRead) -> Result<Value, String> {
        match query {
            PresenterRead::ConversationRefresh { character } => {
                let state = self
                    .conversation
                    .snapshot(character)
                    .await
                    .map_err(|error| error.to_string())?;
                let requests_for_character = state
                    .requests
                    .requests
                    .iter()
                    .filter(|request| request.metadata.character_id == character)
                    .count();
                Ok(json!({
                    "source": "conversation_presenter",
                    "character": character,
                    "version": {
                        "branch_id": state.at.branch_id,
                        "through": state.at.through,
                        "generation": state.selection.generation,
                    },
                    "visible_message_count": state.messages.len(),
                    "chat_revision": state.requests.revision,
                    "character_request_count": requests_for_character,
                    "settings_revision": state.settings.revision,
                    "preset_id": state.preset.0,
                    "refresh_completed": true,
                    "gui_window_rendered": false,
                    "content_included": false,
                    "evidence_level": "presenter_snapshot",
                }))
            }
            PresenterRead::ChatDiagnostics { character, limit } => {
                if !(1..=100).contains(&limit) {
                    return Err("diagnostics limit must be 1..100".into());
                }
                let snapshot = self.chat.snapshot();
                let observed = snapshot
                    .requests
                    .iter()
                    .filter(|request| request.metadata.character_id == character)
                    .collect::<Vec<_>>();
                let requests = observed
                    .iter()
                    .rev()
                    .take(limit)
                    .map(|request| super::ipc::request_status_json(request))
                    .collect::<Vec<_>>();
                Ok(
                    json!({"source":"chat_presenter_snapshot","character":character,
                    "revision":snapshot.revision,"closed":snapshot.closed,
                    "discovery_error_present":snapshot.discovery_error.is_some(),
                    "total_observed":observed.len(),"returned":requests.len(),
                    "requests":requests,"content_included":false,
                    "note":"bounded Presenter retention only; request errors and content are withheld",
                    "evidence_level":"presenter_snapshot"}),
                )
            }
            PresenterRead::CharacterDebug {
                character,
                include_content,
            } => {
                if include_content
                    && std::env::var("NEUROMITA_ENABLE_LOCAL_CHARACTER_DEBUG").as_deref() != Ok("1")
                {
                    return Err("character debug content requires Core host opt-in".into());
                }
                let fields = super::project_character_debug(
                    &self.characters,
                    self.settings.as_ref(),
                    self.prompt_folder.catalog(),
                    character,
                )
                .await?;
                let count = fields.len();
                let output = if include_content {
                    fields
                        .into_iter()
                        .map(|field| json!({"label":field.label,"value":field.value}))
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                Ok(json!({
                    "source":"character_debug_presenter",
                    "character":character,
                    "field_count":count,
                    "fields":output,
                    "content_included":include_content,
                    "requires_host_opt_in":"NEUROMITA_ENABLE_LOCAL_CHARACTER_DEBUG",
                    "evidence_level":"presenter_snapshot",
                }))
            }
            PresenterRead::PromptCatalog { limit } => {
                if !(1..=200).contains(&limit) {
                    return Err("prompt catalog limit must be 1..200".into());
                }
                let inventory = self.prompt_folder.inventory();
                let total = inventory.sets.len();
                let sets = inventory
                    .sets
                    .iter()
                    .take(limit)
                    .map(|set| {
                        json!({
                            "character": set.character,
                            "name": set.name.chars().take(255).collect::<String>(),
                            "author": set.author.chars().take(512).collect::<String>(),
                            "version": set.version.chars().take(128).collect::<String>(),
                            "description": set.description.chars().take(512).collect::<String>(),
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(json!({
                    "source": "prompt_folder_presenter",
                    "configured": inventory.configured,
                    "total": total,
                    "returned": sets.len(),
                    "truncated": total > sets.len(),
                    "read_error_count": inventory.read_error_count,
                    "sets": sets,
                    "live_scan": true,
                    "content_included": true,
                }))
            }
            PresenterRead::PromptFolder { character } => {
                let path = self.prompt_folder.locate(character)?;
                Ok(
                    json!({"source":"prompt_folder_presenter","character":character,
                    "path":path,"opened":false}),
                )
            }
            PresenterRead::MemoryRecall {
                character,
                memory_id,
                query_text,
            } => {
                if query_text.trim().is_empty() || query_text.len() > 4096 {
                    return Err("recall query must contain 1..4096 characters".into());
                }
                let result = crate::api::HistoricalRecallApi::inspect(
                    character,
                    crate::knowledge::forgotten_recall::RecallSelector::Ids(vec![memory_id]),
                    &query_text,
                )
                .await
                .map_err(|error| error.to_string())?;
                Ok(
                    json!({"source":"historical_recall_api","character":character,
                    "memory_id":memory_id,"found":result.is_some(),"text":result}),
                )
            }
            PresenterRead::GraphPreview {
                character,
                memory_id,
                mode,
            } => {
                let mode = match mode {
                    GraphReviewMode::Clear => {
                        crate::knowledge::maintenance::GraphMaintenanceMode::Clear
                    }
                    GraphReviewMode::Reindex => {
                        crate::knowledge::maintenance::GraphMaintenanceMode::Reindex
                    }
                };
                let request_id = Uuid::new_v4();
                let report = self
                    .graph
                    .preview(request_id, character, memory_id, mode)
                    .await?;
                Ok(
                    json!({"source":"graph_maintenance_presenter","request_id":report.request_id,
                    "character":report.character,"relation_count":report.relation_count,
                    "mode":mode_name(mode),"confirmation_required":true}),
                )
            }

            PresenterRead::SpeechTranscript { include_content } => {
                let hub = self.speech_capture.transcripts();
                let ui_route = hub.gui_route();
                let last = hub.latest();
                let mut record = match last {
                    Some(event) => {
                        let mut record = json!({"source":"application_speech_transcript_hub",
                            "revision":event.revision,"character":event.character,
                            "selection_revision":event.selection_revision,
                            "language":event.transcript.language,
                            "text_length":event.transcript.text.chars().count(),
                            "content_included":include_content});
                        if include_content {
                            record["text"] = json!(event.transcript.text);
                        }
                        record
                    }
                    None => json!({"source":"application_speech_transcript_hub",
                        "revision":0,"available":false,"content_included":include_content}),
                };
                record["gui_selected_character"] = json!(ui_route.map(|route| route.0));
                record["gui_selection_revision"] = json!(ui_route.map(|route| route.1));
                Ok(record)
            }
            PresenterRead::SpeechInventory {} => {
                let catalog = self
                    .speech_catalog
                    .as_ref()
                    .ok_or("speech inventory is unavailable without an owned data root")?;
                let inventory = catalog.snapshot().await;
                let options = |records: Vec<
                    crate::ai_runtime::speech_catalog::SpeechModelOption,
                >| {
                    records.into_iter().take(128).map(|m|json!({
                        "model_id":m.id,"name":m.name,"package_detected":m.package_detected,
                        "verified_assets":m.verified_assets,
                        "installed_revisions":m.installed_revisions.into_iter().take(64).collect::<Vec<_>>()
                    })).collect::<Vec<_>>()
                };
                let (devices, device_error) = match self.audio_input.devices().await {
                    Ok(devices) => (
                        devices
                            .into_iter()
                            .take(64)
                            .map(|d| {
                                json!({
                                    "id":d.id,"name":d.name,"backend":d.backend
                                })
                            })
                            .collect::<Vec<_>>(),
                        false,
                    ),
                    Err(_) => (Vec::new(), true),
                };
                Ok(
                    json!({"source":"speech_model_catalog","tts":options(inventory.tts),
                    "asr":options(inventory.asr),"discovery_error_count":inventory.errors.len(),
                    "devices":devices,"device_discovery_failed":device_error,
                    "model_activation_attempted":false,"evidence_level":"presenter_catalog_snapshot"}),
                )
            }
            PresenterRead::NativeModels { refresh } => {
                let native = self
                    .native
                    .as_ref()
                    .ok_or("native AI Hub is not configured")?;
                let snapshot = if refresh {
                    native.refresh().await?
                } else {
                    native.snapshot().await?
                };
                let transfer = native.download_snapshot();
                let authorized = native.authorized_releases();
                let authorized_error = authorized.is_err();
                let digest = |bytes: &[u8; 32]| -> String {
                    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
                };
                let approved_releases = authorized
                    .unwrap_or_default()
                    .into_iter()
                    .take(256)
                    .map(|r| {
                        json!({"model_id":r.model_id,"manifest_sha256":digest(&r.manifest_sha256),
                        "publisher_id":r.publisher_id})
                    })
                    .collect::<Vec<_>>();
                let revisions = snapshot.inventory.revisions.iter().take(256).map(|r| {
                    json!({"model_id":r.model_id,"name":r.display_name,"package_version":r.package_version,
                        "manifest_sha256":digest(&r.manifest_sha256),
                        "backends":r.backends.iter().map(|b| b.as_str()).collect::<Vec<_>>(),
                        "services":r.speech_services})
                }).collect::<Vec<_>>();
                let selections = snapshot.inventory.selections.iter().take(256).map(|s| {
                    json!({"model_id":s.choice.model_id,"manifest_sha256":digest(&s.choice.manifest_sha256),
                        "backend":s.choice.backend.as_str(),"state":format!("{:?}",s.manifest_state)})
                }).collect::<Vec<_>>();
                let active = snapshot
                    .service_workers
                    .iter()
                    .map(|s| {
                        json!({"service":s.service,"model_id":s.choice.model_id,
                        "manifest_sha256":digest(&s.choice.manifest_sha256),
                        "backend":s.choice.backend.as_str(),
                        "worker":s.worker.as_ref().map(|w|json!({"session_id":w.session_id,
                            "phase":format!("{:?}",w.phase),"failed":w.last_failure.is_some()}))})
                    })
                    .collect::<Vec<_>>();
                Ok(
                    json!({"source":"ai_hub_native_service","revision_count":snapshot.inventory.revisions.len(),
                    "revisions":revisions,"selections":selections,"active_service_workers":active,
                    "authorized_releases":approved_releases,"authorized_catalog_error":authorized_error,
                    "download":{"revision":transfer.revision,"operation_id":transfer.operation_id,"phase":transfer.phase,
                        "transferred_bytes":transfer.transferred_bytes,"planned_bytes":transfer.planned_bytes,
                        "succeeded":transfer.succeeded},
                    "native_configured":true,"evidence_level":"application_owner_snapshot"}),
                )
            }
            PresenterRead::Wait {
                target,
                character,
                after_revision,
                after_version,
                timeout_ms,
            } => {
                if !(1..=30000).contains(&timeout_ms) {
                    return Err("wait timeout_ms must be 1..30000".into());
                }
                let deadline = std::time::Duration::from_millis(timeout_ms);
                match target {
                    WaitTarget::RemoteVoice => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err("remote_voice wait requires after_revision without character or version".into());
                        }
                        let owner = self
                            .remote_voice_admin
                            .as_ref()
                            .ok_or("Remote Voice is not configured")?;
                        let threshold = after_revision.expect("validated");
                        let mut updates = owner.observe();
                        let wait = async {
                            loop {
                                let status = updates.borrow_and_update().clone();
                                if status.revision != threshold {
                                    return Ok::<_, String>(
                                        json!({"target":"remote_voice","changed":true,"status":status}),
                                    );
                                }
                                updates
                                    .changed()
                                    .await
                                    .map_err(|_| "Remote Voice observer closed".to_string())?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(
                                json!({"target":"remote_voice","changed":false,"timed_out":true}),
                            ),
                        }
                    }
                    WaitTarget::TelegramAuth => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err("telegram_auth wait requires after_revision without character or version".into());
                        }
                        let owner = self
                            .telegram_admin
                            .as_ref()
                            .ok_or("Telegram administration is not configured")?;
                        let mut updates = owner.observe();
                        let threshold = after_revision.expect("validated");
                        let wait = async {
                            loop {
                                let status = updates.borrow_and_update().clone();
                                if status.revision != threshold {
                                    return Ok::<_, String>(
                                        json!({"target":"telegram_auth","changed":true,"status":status}),
                                    );
                                }
                                updates
                                    .changed()
                                    .await
                                    .map_err(|_| "Telegram observer closed".to_string())?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(
                                json!({"target":"telegram_auth","changed":false,"timed_out":true}),
                            ),
                        }
                    }
                    WaitTarget::SpeechTranscript => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err("speech transcript wait requires after_revision without character or version".into());
                        }
                        let previous = after_revision.expect("validated");
                        let mut updates = self.speech_capture.transcripts().observe();
                        let wait = async {
                            loop {
                                let snapshot = updates.borrow_and_update().clone();
                                let revision = snapshot.as_ref().map_or(0, |value| value.revision);
                                if revision != previous {
                                    return Ok::<_, String>(json!({"target":"speech_transcript",
                                        "changed":true,"revision":revision,
                                        "character":snapshot.as_ref().map(|v|v.character),
                                        "selection_revision":snapshot.as_ref().map(|v|v.selection_revision)}));
                                }
                                updates
                                    .changed()
                                    .await
                                    .map_err(|_| "speech transcript observer closed".to_string())?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(
                                json!({"target":"speech_transcript","changed":false,"timed_out":true}),
                            ),
                        }
                    }
                    WaitTarget::AiHubInstall => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err("ai_hub_install wait needs after_revision without character/version".into());
                        }
                        let owner = self
                            .ai_hub_install
                            .as_ref()
                            .ok_or("AI Hub installation is not configured")?;
                        let mut updates = owner.observe_status();
                        let threshold = after_revision.expect("validated");
                        let wait = async {
                            loop {
                                let status = updates.borrow_and_update().clone();
                                if status.revision != threshold || status.stage == "closed" {
                                    return Ok::<_, String>(
                                        json!({"changed":status.revision!=threshold,
                                        "target":"ai_hub_install","status":status}),
                                    );
                                }
                                updates.changed().await.map_err(|_| {
                                    "AI Hub installation observer stopped".to_string()
                                })?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(
                                json!({"changed":false,"timed_out":true,"target":"ai_hub_install"}),
                            ),
                        }
                    }
                    WaitTarget::NativeDownload => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err("native_download wait needs after_revision without character/version".into());
                        }
                        let native = self
                            .native
                            .as_ref()
                            .ok_or("native AI Hub is not configured")?;
                        let mut watch = native.observe_download();
                        let threshold = after_revision.expect("validated");
                        let wait = async {
                            loop {
                                let state = watch.borrow_and_update().clone();
                                if state.revision != threshold {
                                    return Ok::<_, String>(json!({"changed":true,
                                        "revision":state.revision,"phase":state.phase,
                                        "operation_id":state.operation_id,"succeeded":state.succeeded}));
                                }
                                watch
                                    .changed()
                                    .await
                                    .map_err(|_| "native download observer stopped".to_string())?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(
                                json!({"changed":false,"timed_out":true,"target":"native_download"}),
                            ),
                        }
                    }
                    WaitTarget::Settings => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err(
                                "settings wait needs after_revision and no character/version"
                                    .into(),
                            );
                        }
                        let mut updates = self.settings.observe().map_err(|e| e.to_string())?;
                        let threshold = after_revision.expect("validated");
                        let wait = async {
                            loop {
                                let snapshot = updates.borrow_and_update().clone();
                                if snapshot.closed || snapshot.revision != threshold {
                                    return Ok::<_, String>(
                                        json!({"changed":snapshot.revision!=threshold,
                                        "revision":snapshot.revision,"closed":snapshot.closed}),
                                    );
                                }
                                updates
                                    .changed()
                                    .await
                                    .map_err(|_| "settings observer stopped".to_string())?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => {
                                Ok(json!({"changed":false,"timed_out":true,"target":"settings"}))
                            }
                        }
                    }
                    WaitTarget::Chat => {
                        if character.is_some()
                            || after_version.is_some()
                            || after_revision.is_none()
                        {
                            return Err(
                                "chat wait needs after_revision and no character/version".into()
                            );
                        }
                        let mut updates = self.chat.observe();
                        let threshold = after_revision.expect("validated");
                        let wait = async {
                            loop {
                                let snapshot = updates.borrow_and_update().clone();
                                if snapshot.closed || snapshot.revision != threshold {
                                    return Ok::<_, String>(
                                        json!({"changed":snapshot.revision!=threshold,
                                        "revision":snapshot.revision,"closed":snapshot.closed}),
                                    );
                                }
                                updates
                                    .changed()
                                    .await
                                    .map_err(|_| "chat observer stopped".to_string())?;
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(json!({"changed":false,"timed_out":true,"target":"chat"})),
                        }
                    }
                    WaitTarget::Memory => {
                        if after_revision.is_some()
                            || character.is_none()
                            || after_version.is_none()
                        {
                            return Err("memory wait needs character and after_version".into());
                        }
                        let id = character.expect("validated");
                        let prior = after_version.expect("validated");
                        if prior.len() != 64 || !prior.bytes().all(|c| c.is_ascii_hexdigit()) {
                            return Err("memory version must be a SHA-256 hex token".into());
                        }
                        let mut watcher = self
                            .characters
                            .observe_memories(
                                id,
                                MemoryListQuery {
                                    limit: 1,
                                    ..Default::default()
                                },
                            )
                            .map_err(|e| e.to_string())?;
                        let wait = async {
                            loop {
                                let page = watcher.next().await.map_err(|e| e.to_string())?;
                                let version = memory_version(page.snapshot());
                                if version != prior {
                                    return Ok::<_, String>(
                                        json!({"changed":true,"character":id,"version":version}),
                                    );
                                }
                            }
                        };
                        match tokio::time::timeout(deadline, wait).await {
                            Ok(result) => result,
                            Err(_) => Ok(
                                json!({"changed":false,"timed_out":true,"target":"memory","character":id}),
                            ),
                        }
                    }
                }
            }
            PresenterRead::Settings { area, character } => {
                if character.is_some() {
                    return Err("settings areas do not accept character".into());
                }
                let snapshot = self.settings.snapshot().map_err(|e| e.to_string())?;
                let v = &snapshot.values;
                let value = match area {
                    SettingsArea::Interface => json!(v.interface),
                    SettingsArea::Voice => json!(v.voice),
                    SettingsArea::Memory => json!(v.memory),
                    SettingsArea::Capture => json!(v.capture),
                    SettingsArea::Onboarding => json!(v.onboarding),
                    SettingsArea::HistoryGeneration => json!(v.history_generation),
                    SettingsArea::DocumentGeneration => json!(v.document_generation),
                    SettingsArea::Tts => {
                        json!({"enabled":v.speech.tts.enabled,"backend":v.speech.tts.backend,
                        "local_model_id":v.speech.tts.local_model_id,"local_language":v.speech.tts.local_language,
                        "local_auto_load":v.speech.tts.local_auto_load,"api_model":v.speech.tts.api_model,
                        "model":v.speech.model_for(crate::ai_runtime::AiService::Tts)})
                    }
                    SettingsArea::Asr => json!({"preferences":v.speech.asr,
                        "model":v.speech.model_for(crate::ai_runtime::AiService::Asr)}),
                    SettingsArea::Tools => {
                        json!({"enabled":v.tools.enabled,"calculator":v.tools.calculator,
                        "memory_search":v.tools.memory_search,"max_depth":v.tools.max_depth,
                        "web_enabled":v.tools.web.enabled,"web_search_enabled":v.tools.web.search_enabled,
                        "web_reader_enabled":v.tools.web.reader_enabled,
                        "credentials_and_endpoints_withheld":true})
                    }
                    SettingsArea::Persistence => {
                        json!({"persisted_revision":snapshot.persisted_revision,
                        "persistence":format!("{:?}",snapshot.persistence)})
                    }
                };
                Ok(
                    json!({"source":"settings_presenter","area":area,"revision":snapshot.revision,
                    "closed":snapshot.closed,"values":value,"evidence_level":"presenter_snapshot"}),
                )
            }
            PresenterRead::CharacterProfile {
                character,
                include_content,
            } => {
                let profile = self
                    .settings
                    .characters
                    .profile(character)
                    .map_err(|e| e.to_string())?;
                let s = profile.settings();
                Ok(
                    json!({"source":"characters_settings_presenter","character":character,"revision":profile.revision(),
                    "presets":s.presets,"has_prompt_set":s.prompt_set.is_some(),
                    "prompt_set":include_content.then(||s.prompt_set.as_deref()).flatten()}),
                )
            }
            PresenterRead::Memories {
                character,
                offset,
                limit,
                include_content,
                state,
            } => {
                if !(1..=100).contains(&limit) || offset > 100_000 {
                    return Err("invalid memory page".into());
                }
                let snapshot = self
                    .characters
                    .memories(character)
                    .await
                    .map_err(|e| e.to_string())?;
                let version = memory_version(&snapshot);
                let page = MemoryListPage::from_snapshot(
                    snapshot,
                    MemoryListQuery {
                        text: None,
                        state,
                        kind: None,
                        offset,
                        limit,
                    },
                )
                .map_err(|e| e.to_string())?;
                let records=page.records().iter().map(|r|{
                    let mut item=json!({"id":r.id,"state":r.state,"kind":r.kind,"priority":r.priority,
                        "protected":r.protected,"expires_at_unix_ms":r.expires_at_unix_ms,
                        "content_length":r.content.chars().count()});
                    if include_content {item["content"]=json!(r.content);item["entities"]=json!(r.entities);}
                    item
                }).collect::<Vec<_>>();
                Ok(
                    json!({"source":"characters_presenter","character":character,"version":version,
                    "total_matches":page.total_matches(),"next_offset":page.next_offset(),
                    "records":records,"content_included":include_content}),
                )
            }
            PresenterRead::History {
                character,
                offset,
                limit,
                include_content,
                version,
            } => {
                if !(1..=100).contains(&limit) || offset > 100_000 {
                    return Err("invalid history page".into());
                }
                let state = self
                    .conversation
                    .snapshot(character)
                    .await
                    .map_err(|e| e.to_string())?;
                let fence = TimelineFence {
                    at: state.at,
                    selection: state.selection,
                };
                if version.is_some_and(|v| !v.matches(fence)) {
                    return Err("stale conversation version".into());
                }
                let page = self
                    .conversation
                    .history_page(character, state.at, state.selection, offset)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut messages = page.messages;
                messages.truncate(limit);
                Ok(
                    json!({"source":"conversation_presenter","character":character,
                    "version":{"branch_id":state.at.branch_id,"through":state.at.through,"generation":state.selection.generation},
                    "total":page.total,"offset":offset,"returned":messages.len(),
                    "messages":messages_json(&messages,include_content),"content_included":include_content}),
                )
            }
            PresenterRead::ConversationPreview { character, change } => {
                let preview = self
                    .conversation
                    .preview_change(character, change.into_change())
                    .await
                    .map_err(|e| e.to_string())?;
                let fence = preview.timeline_fence();
                Ok(
                    json!({"source":"conversation_presenter","character":character,"affected_messages":preview.message_count(),
                    "version":{"branch_id":fence.at.branch_id,"through":fence.at.through,"generation":fence.selection.generation},
                    "note":"preview only; apply validates the same timeline version"}),
                )
            }
            PresenterRead::RegeneratePreview {
                character,
                event_id,
                version,
            } => {
                let preview = self
                    .conversation
                    .preview_regenerate(character, EventId(event_id), version.fence())
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"conversation_presenter","accepted_message":preview.accepted_message().0,
                    "accepted_request":preview.accepted_request().0,"original_input":preview.original_input().0,
                    "visible_downstream":preview.visible_downstream(),"regeneration_executed":false}),
                )
            }
        }
    }

    pub(crate) async fn admin_write(&self, change: PresenterWrite) -> Result<Value, String> {
        match change {
            PresenterWrite::AttachFile {
                character,
                version,
                text,
                source,
            } => {
                if std::env::var_os("NEUROMITA_ENABLE_LOCAL_FILE_ATTACH").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
                {
                    return Err(
                        "local file attachment requires NEUROMITA_ENABLE_LOCAL_FILE_ATTACH=1"
                            .into(),
                    );
                }
                if source.len() > 4096 || !std::path::Path::new(&source).is_absolute() {
                    return Err(
                        "attachment source must be an absolute path up to 4096 bytes".into(),
                    );
                }
                if text.len() > 65536 {
                    return Err("attachment text exceeds 64 KiB".into());
                }
                let id = self.conversation.attach_file(
                    character,
                    text,
                    std::path::Path::new(&source),
                    version.fence(),
                )?;
                Ok(
                    json!({"source":"conversation_presenter","event_id":id.0,"character":character,
                    "attachment_committed":true}),
                )
            }
            PresenterWrite::GuideRetry {} => {
                let change = self
                    .guide_persistence
                    .retry()
                    .await
                    .map_err(|e| e.to_string())?;
                let snapshot = self.settings.snapshot().map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"guide_persistence_presenter","change":format!("{change:?}"),
                    "revision":snapshot.revision,"persisted_revision":snapshot.persisted_revision}),
                )
            }
            PresenterWrite::SettingsRetry {} => {
                self.settings
                    .retry_failed_save()
                    .map_err(|e| e.to_string())?;
                let snapshot = self.settings.snapshot().map_err(|e| e.to_string())?;
                Ok(json!({
                    "source": "settings_presenter",
                    "retry_requested": true,
                    "revision": snapshot.revision,
                    "persisted_revision": snapshot.persisted_revision,
                    "disk_save_confirmed": snapshot.persisted_revision == Some(snapshot.revision)
                }))
            }
            PresenterWrite::PromptFolderOpen { character } => {
                if std::env::var_os("NEUROMITA_ENABLE_LOCAL_HOST_ACTIONS").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
                {
                    return Err(
                        "host shell actions require NEUROMITA_ENABLE_LOCAL_HOST_ACTIONS=1".into(),
                    );
                }
                self.prompt_folder.open(character)?;
                Ok(json!({"source":"prompt_folder_presenter","character":character,"opened":true}))
            }
            PresenterWrite::GraphApply {
                character,
                request_id,
            } => {
                if std::env::var_os("NEUROMITA_ENABLE_LOCAL_GRAPH_MAINTENANCE").as_deref()
                    != Some(std::ffi::OsStr::new("1"))
                {
                    return Err(
                        "graph maintenance requires NEUROMITA_ENABLE_LOCAL_GRAPH_MAINTENANCE=1"
                            .into(),
                    );
                }
                self.graph.apply(request_id, character).await?;
                Ok(
                    json!({"source":"graph_maintenance_presenter","request_id":request_id,
                    "applied":true,"confirmed":true}),
                )
            }
            PresenterWrite::GraphAbort { request_id } => {
                let aborted = self.graph.abort(request_id).await;
                Ok(
                    json!({"source":"graph_maintenance_presenter","request_id":request_id,
                    "aborted":aborted}),
                )
            }

            PresenterWrite::Settings { field, value } => {
                let p = &self.settings;
                match field {
                    SettingsField::Language => p
                        .interface
                        .language
                        .set(serde_json::from_value::<Language>(value).map_err(|e| e.to_string())?),
                    SettingsField::FontSize => p.interface.set_font_size(
                        serde_json::from_value::<u16>(value).map_err(|e| e.to_string())?,
                    ),
                    SettingsField::HidePrivate => p
                        .interface
                        .hide_private
                        .set(serde_json::from_value::<bool>(value).map_err(|e| e.to_string())?),
                    SettingsField::StructuredDisplay => p.interface.structured_display.set(
                        serde_json::from_value::<StructuredDisplayMode>(value)
                            .map_err(|e| e.to_string())?,
                    ),
                    SettingsField::VoiceVolume => p.voice.volume.set(
                        serde_json::from_value::<crate::settings::Volume>(value)
                            .map_err(|e| e.to_string())?,
                    ),
                    SettingsField::VoiceInLauncher => p
                        .voice
                        .in_launcher
                        .set(serde_json::from_value::<bool>(value).map_err(|e| e.to_string())?),
                    SettingsField::VoiceInGame => p
                        .voice
                        .in_game
                        .set(serde_json::from_value::<bool>(value).map_err(|e| e.to_string())?),
                    SettingsField::VoiceName => p
                        .voice
                        .voice
                        .set(serde_json::from_value::<String>(value).map_err(|e| e.to_string())?),
                    SettingsField::MemoryDedupEnabled => {
                        let mut s = p.memory.state.get();
                        s.model_add_dedup_enabled =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        p.memory.state.set(s)
                    }
                    SettingsField::MemoryDedupThreshold => {
                        let mut s = p.memory.state.get();
                        s.threshold_per_mille =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        p.memory.state.set(s)
                    }
                    SettingsField::SectionVisibility => {
                        #[derive(Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct VisibilityChange {
                            area: crate::settings::SettingsArea,
                            visible: bool,
                        }
                        let change: VisibilityChange =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        p.interface.set_visible(change.area, change.visible)
                    }
                }
                .map_err(|e| e.to_string())?;
                let snapshot = p.snapshot().map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"settings_presenter","field":field,"revision":snapshot.revision,
                    "persisted_revision":snapshot.persisted_revision,"disk_save_confirmed":snapshot.persisted_revision==Some(snapshot.revision)}),
                )
            }
            PresenterWrite::CharacterProfile {
                character,
                expected_revision,
                profile,
            } => {
                let current = self
                    .settings
                    .characters
                    .profile(character)
                    .map_err(|e| e.to_string())?;
                if current.revision() != expected_revision {
                    return Err("stale character settings revision".into());
                }
                self.validate_character_profile(character, &profile)?;
                let saved = self
                    .settings
                    .characters
                    .save_profile(&current, profile)
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"characters_settings_presenter","character":character,"revision":saved.revision()}),
                )
            }
            PresenterWrite::CharacterPresetAssignment {
                character,
                expected_revision,
                slot,
                selection,
            } => {
                let current = self
                    .settings
                    .characters
                    .profile(character)
                    .map_err(|e| e.to_string())?;
                if current.revision() != expected_revision {
                    return Err("stale character settings revision".into());
                }
                let mut edited = current.settings().clone();
                match slot {
                    CharacterPresetSlot::Reply => edited.presets.reply = selection,
                    CharacterPresetSlot::React => edited.presets.react = selection,
                    CharacterPresetSlot::Initiate => edited.presets.initiate = selection,
                    CharacterPresetSlot::All => {
                        edited.presets.reply = selection;
                        edited.presets.react = selection;
                        edited.presets.initiate = selection;
                    }
                }
                self.validate_character_profile(character, &edited)?;
                let saved = self
                    .settings
                    .characters
                    .save_profile(&current, edited)
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"characters_settings_presenter","character":character,
                    "slot":slot,"revision":saved.revision(),"presets":saved.settings().presets}),
                )
            }
            PresenterWrite::CharacterPromptSelection {
                character,
                expected_revision,
                prompt_set,
            } => {
                let current = self
                    .settings
                    .characters
                    .profile(character)
                    .map_err(|e| e.to_string())?;
                if current.revision() != expected_revision {
                    return Err("stale character settings revision".into());
                }
                let mut edited = current.settings().clone();
                edited.prompt_set = prompt_set;
                self.validate_character_profile(character, &edited)?;
                let saved = self
                    .settings
                    .characters
                    .save_profile(&current, edited)
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"characters_settings_presenter","character":character,
                    "revision":saved.revision(),"prompt_set":saved.settings().prompt_set}),
                )
            }
            PresenterWrite::Memory {
                character,
                expected_version,
                commands,
            } => {
                if !(1..=32).contains(&commands.len()) {
                    return Err("memory edits require 1..32 commands".into());
                }
                let snapshot = self
                    .characters
                    .memories(character)
                    .await
                    .map_err(|e| e.to_string())?;
                if memory_version(&snapshot) != expected_version {
                    return Err("stale memory version".into());
                }
                let changes = commands.into_iter().map(Into::into).collect();
                let result = self
                    .characters
                    .edit_memories(character, snapshot, changes)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"characters_presenter","character":character,"applied":result.applied.len(),
                    "version":memory_version(&result.snapshot)}),
                )
            }
            PresenterWrite::Conversation {
                character,
                version,
                edit,
            } => {
                let current = self
                    .conversation
                    .snapshot(character)
                    .await
                    .map_err(|e| e.to_string())?;
                if !version.matches(TimelineFence {
                    at: current.at,
                    selection: current.selection,
                }) {
                    return Err("stale conversation version".into());
                }
                let preview = self
                    .conversation
                    .preview_change(character, edit.into_change())
                    .await
                    .map_err(|e| e.to_string())?;
                let affected = preview.message_count();
                self.conversation
                    .apply_change(preview)
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"conversation_presenter","character":character,"affected_messages":affected,"applied":true}),
                )
            }
            PresenterWrite::RetryFailed {
                character,
                request_id,
            } => {
                let request = self
                    .conversation
                    .retry_failed(character, RequestId(request_id))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"conversation_presenter","character":character,"request_id":request.id().0,
                    "admitted":true,"note":"use inspect_request to observe terminal status"}),
                )
            }
        }
    }
}
