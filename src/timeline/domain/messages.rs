use std::collections::HashMap;

use crate::identity::{CharacterId, EventId};

use super::{
    character_view,
    model::{EventKind, TimelineFact},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedMessage {
    pub event_id: EventId,
    pub external_message_id: Option<String>,
    pub sequence: u64,
    pub learned_at: u64,
    pub role: MessageRole,
    pub speaker: Option<String>,
    pub content: String,
}

pub fn character_messages(facts: &[TimelineFact], character: CharacterId) -> Vec<ProjectedMessage> {
    let mut known = HashMap::<EventId, ProjectedMessage>::new();
    let mut visible = HashMap::<EventId, ProjectedMessage>::new();
    for item in character_view(facts, character) {
        let fact = &item.fact;
        let draft = &fact.draft;
        let direct = draft.observers.contains(&character) && item.learned_at == fact.sequence;
        if direct && draft.kind == EventKind::CharacterContextReset {
            known.clear();
            visible.clear();
            continue;
        }
        if let Some(role) = match draft.kind {
            EventKind::PlayerMessage | EventKind::CharacterObservation => Some(MessageRole::User),
            EventKind::CharacterMessage => {
                Some(if draft.actor.as_deref() == Some(&character.to_string()) {
                    MessageRole::Assistant
                } else {
                    MessageRole::User
                })
            }
            EventKind::SessionNote => Some(MessageRole::System),
            _ => None,
        } {
            let content = draft
                .payload
                .get("content")
                .and_then(|value| value.as_str())
                .or_else(|| draft.payload.get("message")?.get("content")?.as_str())
                .unwrap_or("")
                .to_owned();
            let message = ProjectedMessage {
                event_id: draft.id,
                external_message_id: draft.external_message_id.clone(),
                sequence: fact.sequence,
                learned_at: item.learned_at,
                role,
                speaker: if draft.kind == EventKind::CharacterObservation {
                    Some("World".into())
                } else {
                    draft.actor.clone()
                },
                content,
            };
            known.insert(draft.id, message.clone());
            visible.insert(draft.id, message);
        } else if direct && draft.kind == EventKind::MessageHidden {
            if let Some(ids) = draft
                .payload
                .get("event_ids")
                .and_then(|value| value.as_array())
            {
                for id in ids {
                    if let Some(id) = id.as_str().and_then(|id| uuid::Uuid::parse_str(id).ok()) {
                        visible.remove(&EventId(id));
                    }
                }
            }
        } else if direct
            && matches!(
                draft.kind,
                EventKind::MessageRevised | EventKind::MessageRestored
            )
            && let Some(target) = draft
                .payload
                .get("event_id")
                .and_then(|value| value.as_str())
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
            && let Some(original) = known.get_mut(&EventId(target))
        {
            if draft.kind == EventKind::MessageRevised
                && let Some(content) = draft
                    .payload
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(|value| value.as_str())
            {
                original.content = content.to_owned();
            }
            visible.insert(EventId(target), original.clone());
        }
    }
    let mut messages: Vec<_> = visible.into_values().collect();
    messages.sort_by_key(|message| (message.learned_at, message.sequence));
    messages
}
