use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::watch;

use super::{BoundProperty, CharactersPresenter, ChatModel, ChatPresenter, PresenterError};
use crate::{
    character::{CharacterError, CharacterId, CharacterState},
    generation::{PresetCatalog, PresetCatalogError, PresetCatalogSnapshot, PresetId},
    request::{CancelResult, RequestHandle, RequestId, TimelineFence},
    settings::{PresetSelection, SettingsError, SettingsOwner, SettingsSnapshot, Volume},
    timeline::{
        ContextAnchor, SessionTimeline,
        domain::{MessageRole, ProjectedMessage},
        selection::TimelineSelection,
    },
};

#[derive(Debug, Error)]
pub enum ConversationError {
    #[error(transparent)]
    Character(#[from] CharacterError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Preset(#[from] PresetCatalogError),
    #[error("timeline is unavailable")]
    TimelineUnavailable,
    #[error(transparent)]
    Timeline(#[from] crate::timeline::domain::TimelineError),
    #[error("conversation changed after confirmation was prepared; review the action again")]
    StalePreview,
    #[error("message is not visible to the selected character at this timeline position")]
    MessageNotVisible,
    #[error("only non-empty text-only visible messages can be revised")]
    InvalidMessageEdit,
    #[error("failed input is no longer the last visible player message")]
    RetrySuperseded,
    #[error("regenerate target is not the latest visible canonically accepted reply to a player event")]
    RegenerateUnavailable,
    #[error(transparent)]
    Presenter(#[from] PresenterError),
}

#[derive(Debug, Clone)]
pub enum ConversationChange {
    ClearMessages,
    ResetContext,
    HideMessage(crate::identity::EventId),
    ReviseMessage(crate::identity::EventId, String),
}

pub struct ConversationChangePreview {
    character: CharacterId,
    at: ContextAnchor,
    selection: TimelineSelection,
    operation: ConversationChange,
    targets: Vec<crate::identity::EventId>,
}

impl ConversationChangePreview {
    pub fn operation(&self) -> ConversationChange {
        self.operation.clone()
    }
    pub fn message_count(&self) -> usize {
        self.targets.len()
    }
}

/// Read-only accepted-reply provenance bound to one exact Timeline snapshot.
/// It does not mutate, enqueue, or promise that destructive regeneration is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegeneratePreview {
    character: CharacterId,
    accepted_message: crate::identity::EventId,
    accepted_request: RequestId,
    original_input: crate::identity::EventId,
    at: ContextAnchor,
    selection: TimelineSelection,
    visible_downstream: usize,
}

impl RegeneratePreview {
    pub fn character(&self) -> CharacterId {
        self.character
    }

    pub fn accepted_message(&self) -> crate::identity::EventId {
        self.accepted_message
    }

    pub fn accepted_request(&self) -> RequestId {
        self.accepted_request
    }

    pub fn original_input(&self) -> crate::identity::EventId {
        self.original_input
    }

    pub fn timeline_fence(&self) -> TimelineFence {
        TimelineFence {
            at: self.at,
            selection: self.selection,
        }
    }

    pub fn visible_downstream(&self) -> usize {
        self.visible_downstream
    }
}

pub struct ConversationHistoryPage {
    pub character: CharacterId,
    pub at: ContextAnchor,
    pub selection: TimelineSelection,
    pub offset: usize,
    pub total: usize,
    pub messages: Vec<ProjectedMessage>,
}

pub struct ConversationSnapshot {
    pub character: CharacterId,
    pub at: ContextAnchor,
    pub selection: TimelineSelection,
    pub state: CharacterState,
    pub messages: Vec<ProjectedMessage>,
    pub requests: Arc<ChatModel>,
    pub settings: Arc<SettingsSnapshot>,
    pub preset: PresetId,
    pub preset_name: Option<String>,
    pub model: Result<String, PresetCatalogError>,
}

#[derive(Clone)]
pub struct ConversationPresenter {
    chat: ChatPresenter,
    characters: CharactersPresenter,
    timeline: Arc<Mutex<SessionTimeline>>,
    settings: Arc<SettingsOwner>,
    presets: Arc<PresetCatalog>,
    volume: BoundProperty<Volume>,
}

impl ConversationPresenter {
    pub async fn preview_change(
        &self,
        character: CharacterId,
        operation: ConversationChange,
    ) -> Result<ConversationChangePreview, ConversationError> {
        let (at, selection) = {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ConversationError::TimelineUnavailable)?;
            (timeline.anchor(), timeline.selection())
        };
        let view = self.characters.context(character, at).await?;
        let visible = view.view.messages();
        let targets = match &operation {
            ConversationChange::HideMessage(id) => {
                if !visible.iter().any(|message| message.event_id == *id) {
                    return Err(ConversationError::MessageNotVisible);
                }
                vec![*id]
            }
            ConversationChange::ReviseMessage(id, content) => {
                let message = visible
                    .iter()
                    .find(|message| message.event_id == *id)
                    .ok_or(ConversationError::MessageNotVisible)?;
                if content.trim().is_empty()
                    || !message
                        .fields
                        .get("content")
                        .is_some_and(serde_json::Value::is_string)
                {
                    return Err(ConversationError::InvalidMessageEdit);
                }
                vec![*id]
            }
            _ => visible
                .into_iter()
                .map(|message| message.event_id)
                .collect(),
        };
        Ok(ConversationChangePreview {
            character,
            at,
            selection,
            operation,
            targets,
        })
    }

    pub fn apply_change(
        &self,
        preview: ConversationChangePreview,
    ) -> Result<(), ConversationError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| ConversationError::TimelineUnavailable)?;
        if timeline.anchor() != preview.at || timeline.selection() != preview.selection {
            return Err(ConversationError::StalePreview);
        }
        let id = crate::identity::EventId(uuid::Uuid::new_v4());
        match preview.operation {
            ConversationChange::ClearMessages if !preview.targets.is_empty() => {
                timeline.change_message(
                    preview.selection,
                    preview.character,
                    id,
                    crate::timeline::message_actions::MessageAction::Hide {
                        targets: preview.targets,
                    },
                )?;
            }
            ConversationChange::ClearMessages => {}
            ConversationChange::HideMessage(_) => {
                timeline.change_message(
                    preview.selection,
                    preview.character,
                    id,
                    crate::timeline::message_actions::MessageAction::Hide {
                        targets: preview.targets,
                    },
                )?;
            }
            ConversationChange::ResetContext => {
                timeline.reset_character_context(preview.selection, preview.character, id)?;
            }
            ConversationChange::ReviseMessage(target, content) => {
                let mut changes = serde_json::Map::new();
                changes.insert("content".into(), serde_json::Value::String(content));
                timeline.change_message(
                    preview.selection,
                    preview.character,
                    id,
                    crate::timeline::message_actions::MessageAction::Revise { target, changes },
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn new(
        chat: ChatPresenter,
        characters: CharactersPresenter,
        timeline: Arc<Mutex<SessionTimeline>>,
        settings: Arc<SettingsOwner>,
        presets: Arc<PresetCatalog>,
    ) -> Self {
        let volume = super::SettingsPresenter::new(Arc::clone(&settings))
            .voice
            .volume;
        Self {
            chat,
            characters,
            timeline,
            settings,
            presets,
            volume,
        }
    }

    pub fn characters(&self) -> &'static [CharacterId] {
        self.characters.available()
    }

    pub fn reply(
        &self,
        character: CharacterId,
        text: String,
    ) -> Result<RequestHandle, PresenterError> {
        self.chat.reply(character, text)
    }

    /// Re-admits the original player EventId without appending another user message.
    /// The immutable view is checked before admission and the same exact branch,
    /// generation, and head are asserted again inside the admission lock.
    pub async fn retry_failed(
        &self,
        character: CharacterId,
        source: RequestId,
    ) -> Result<RequestHandle, ConversationError> {
        let event = self.chat.retry_target(character, source)?;
        let (at, selection) = {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ConversationError::TimelineUnavailable)?;
            (timeline.anchor(), timeline.selection())
        };
        let context = self.characters.context(character, at).await?;
        if !context.view.messages().last().is_some_and(|message| {
            message.event_id == event
                && message.role == MessageRole::User
                && message.speaker.as_deref() == Some("player")
        }) {
            return Err(ConversationError::RetrySuperseded);
        }
        Ok(self
            .chat
            .retry_failed_at(character, source, TimelineFence { at, selection })?)
    }

    /// Resolve the original Reply(EventId) from an accepted Timeline receipt.
    /// Regenerate is deliberately not admitted here: downstream effects and
    /// cross-observer branch semantics require a separate atomic owner operation.
    pub async fn preview_regenerate(
        &self,
        character: CharacterId,
        accepted: crate::identity::EventId,
        expected: TimelineFence,
    ) -> Result<RegeneratePreview, ConversationError> {
        {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ConversationError::TimelineUnavailable)?;
            if timeline.anchor() != expected.at || timeline.selection() != expected.selection {
                return Err(ConversationError::StalePreview);
            }
        }

        let captured = self.characters.context(character, expected.at).await?;
        let visible = captured.view.messages();
        let latest = visible
            .iter()
            .rev()
            .find(|message| {
                message.role == MessageRole::Assistant
                    && message.speaker.as_deref() == Some(character.as_str())
            })
            .ok_or(ConversationError::RegenerateUnavailable)?;
        if latest.event_id != accepted {
            return Err(ConversationError::RegenerateUnavailable);
        }
        let request = latest
            .request_id
            .ok_or(ConversationError::RegenerateUnavailable)?;

        let timeline = self
            .timeline
            .lock()
            .map_err(|_| ConversationError::TimelineUnavailable)?;
        if timeline.anchor() != expected.at || timeline.selection() != expected.selection {
            return Err(ConversationError::StalePreview);
        }
        let receipt = timeline
            .accepted_reply_at(character, request, expected.at)?
            .filter(|fact| fact.draft.id == accepted)
            .ok_or(ConversationError::RegenerateUnavailable)?;
        let causes = receipt
            .draft
            .links
            .iter()
            .filter(|link| link.relation == crate::timeline::domain::CausalRelation::ReplyTo)
            .map(|link| link.event_id)
            .collect::<Vec<_>>();
        let [original_input] = causes.as_slice() else {
            return Err(ConversationError::RegenerateUnavailable);
        };
        let original_input = *original_input;
        let canonical_reply_to = original_input.0.to_string();
        if receipt.draft.payload.get("reply_to").and_then(serde_json::Value::as_str)
            != Some(canonical_reply_to.as_str())
        {
            return Err(ConversationError::RegenerateUnavailable);
        }
        let cause = timeline.reaction_cause_at(
            character,
            crate::request::TurnIntent::Reply(original_input),
            expected.at,
        )?;
        if !cause.is_some_and(|fact| {
            fact.draft.kind == crate::timeline::domain::EventKind::PlayerMessage
                && fact.draft.actor.as_deref() == Some("player")
        }) {
            return Err(ConversationError::RegenerateUnavailable);
        }
        if !visible.iter().any(|message| {
            message.event_id == original_input
                && message.role == MessageRole::User
                && message.speaker.as_deref() == Some("player")
                && message.sequence < receipt.sequence
        }) {
            return Err(ConversationError::RegenerateUnavailable);
        }
        Ok(RegeneratePreview {
            character,
            accepted_message: accepted,
            accepted_request: request,
            original_input,
            at: expected.at,
            selection: expected.selection,
            visible_downstream: visible
                .iter()
                .filter(|message| message.sequence > receipt.sequence)
                .count(),
        })
    }

    pub fn cancel(&self, request: RequestId) -> Result<CancelResult, PresenterError> {
        self.chat.cancel(request)
    }

    pub fn set_volume(&self, value: Volume) -> Result<(), SettingsError> {
        self.volume.set(value)
    }

    pub fn observe_requests(&self) -> watch::Receiver<Arc<ChatModel>> {
        self.chat.observe()
    }

    pub fn observe_settings(
        &self,
    ) -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        self.settings.observe()
    }

    pub fn observe_presets(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.presets.observe()
    }

    pub fn observe_selection(
        &self,
    ) -> Result<watch::Receiver<TimelineSelection>, ConversationError> {
        Ok(self
            .timeline
            .lock()
            .map_err(|_| ConversationError::TimelineUnavailable)?
            .observe_selection())
    }

    /// Reads at most 200 older visible messages from one immutable Timeline prefix.
    /// Both the branch head and its selection generation must match at both ends
    /// of the asynchronous capture; a stale page is never silently rebased.
    pub async fn history_page(
        &self,
        character: CharacterId,
        at: ContextAnchor,
        expected: TimelineSelection,
        offset: usize,
    ) -> Result<ConversationHistoryPage, ConversationError> {
        {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ConversationError::TimelineUnavailable)?;
            if timeline.anchor() != at || timeline.selection() != expected {
                return Err(ConversationError::StalePreview);
            }
        }
        let captured = self.characters.context(character, at).await?;
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| ConversationError::TimelineUnavailable)?;
        if timeline.anchor() != at || timeline.selection() != expected {
            return Err(ConversationError::StalePreview);
        }
        let total = captured.view.count();
        Ok(ConversationHistoryPage {
            character,
            at,
            selection: expected,
            offset,
            total,
            messages: captured.view.message_page(200, offset),
        })
    }

    /// Returns a fixed-prefix character view and independent request observations.
    /// A terminal request alone does not prove membership in the selected branch.
    pub async fn snapshot(
        &self,
        character: CharacterId,
    ) -> Result<ConversationSnapshot, ConversationError> {
        let requests = self.chat.snapshot();
        let (at, selection) = {
            let timeline = self
                .timeline
                .lock()
                .map_err(|_| ConversationError::TimelineUnavailable)?;
            (timeline.anchor(), timeline.selection())
        };
        let view = self.characters.context(character, at).await?;
        let settings = self.settings.snapshot_all()?;
        let assigned = settings
            .values
            .characters
            .for_character(character)
            .presets
            .reply;
        let preset = match assigned {
            PresetSelection::Active => settings.values.presets.active,
            PresetSelection::Specific(id) => id,
        };
        let model = self
            .presets
            .get(preset)
            .and_then(|value| value.ok_or(PresetCatalogError::NotFound(preset)))
            .map(|resolved| resolved.template().model().to_owned());
        let preset_name = self
            .presets
            .observe()
            .borrow()
            .entries
            .get(&preset)
            .map(|record| record.name.clone());
        Ok(ConversationSnapshot {
            character,
            at,
            selection,
            state: view.state,
            messages: view.view.messages(),
            requests,
            settings,
            preset,
            preset_name,
            model,
        })
    }
}
