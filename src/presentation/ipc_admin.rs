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
    settings::{CharacterSettings, Language, StructuredDisplayMode},
    timeline::selection::TimelineSelection,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "query", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PresenterRead {
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
    pub(crate) async fn admin_read(&self, query: PresenterRead) -> Result<Value, String> {
        match query {
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
                        json!({"configured":true,"note":"tool credentials and arbitrary payloads withheld"})
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
                let saved = self
                    .settings
                    .characters
                    .save_profile(&current, profile)
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({"source":"characters_settings_presenter","character":character,"revision":saved.revision()}),
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
