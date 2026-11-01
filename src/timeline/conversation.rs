use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::identity::{CharacterId, EventId};

use super::domain::{
    AppendGuard, BranchId, CausalLink, CausalRelation, EventDraft, EventKind, TimelineError,
    TimelineFact, TimelineRepository,
};

/// A speaker's stable identity, independent of how each listener renders its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationSpeaker {
    Player,
    Character(CharacterId),
}

/// One conversation message, recorded once for all characters who heard it.
/// Participants describe the dialogue roster; witnesses describe actual knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub id: EventId,
    pub message_id: String,
    pub speaker: ConversationSpeaker,
    pub target: Option<CharacterId>,
    pub participants: BTreeSet<CharacterId>,
    pub witnesses: BTreeSet<CharacterId>,
    pub content: Value,
    pub links: Vec<CausalLink>,
    pub occurred_at_unix_ms: u64,
}

impl ConversationMessage {
    pub fn new(
        message_id: impl Into<String>,
        speaker: ConversationSpeaker,
        content: Value,
        witnesses: impl IntoIterator<Item = CharacterId>,
    ) -> Self {
        let kind = match speaker {
            ConversationSpeaker::Player => EventKind::PlayerMessage,
            ConversationSpeaker::Character(_) => EventKind::CharacterMessage,
        };
        let draft = EventDraft::new(kind);
        Self {
            id: draft.id,
            message_id: message_id.into(),
            speaker,
            target: None,
            participants: BTreeSet::new(),
            witnesses: witnesses.into_iter().collect(),
            content,
            links: Vec::new(),
            occurred_at_unix_ms: draft.occurred_at_unix_ms,
        }
    }

    fn into_draft(self) -> Result<EventDraft, TimelineError> {
        if self.message_id.is_empty()
            || self.message_id.trim() != self.message_id
            || self.message_id.len() > 256
        {
            return Err(TimelineError::Invalid(
                "invalid conversation message identity",
            ));
        }
        if self.content.is_null() {
            return Err(TimelineError::Invalid(
                "conversation message requires content",
            ));
        }
        let (kind, actor) = match self.speaker {
            ConversationSpeaker::Player => (EventKind::PlayerMessage, "player".to_owned()),
            ConversationSpeaker::Character(character) => {
                (EventKind::CharacterMessage, character.to_string())
            }
        };
        let mut witnesses = self.witnesses;
        if let ConversationSpeaker::Character(character) = self.speaker {
            witnesses.insert(character);
        }
        if witnesses.is_empty() {
            return Err(TimelineError::Invalid(
                "conversation message requires witnesses",
            ));
        }
        let mut draft = EventDraft::new(kind);
        draft.id = self.id;
        draft.actor = Some(actor);
        draft.observers = witnesses;
        draft.external_message_id = Some(self.message_id.clone());
        draft.links = self.links;
        draft.occurred_at_unix_ms = self.occurred_at_unix_ms;
        draft
            .payload
            .insert("message_id".into(), json!(self.message_id));
        draft.payload.insert("content".into(), self.content.clone());
        draft.payload.insert(
            "message".into(),
            json!({
                "content": self.content,
                "target": self.target.map(|id| id.to_string()),
                "participants": self.participants.iter().map(ToString::to_string).collect::<Vec<_>>(),
            }),
        );
        Ok(draft)
    }
}

/// Commit a single canonical message. Replayed identities cannot change the
/// original audience, payload, author, source time or causal links.
pub fn append_conversation_message(
    repository: &mut dyn TimelineRepository,
    branch: BranchId,
    expected_head: u64,
    guard: AppendGuard,
    message: ConversationMessage,
) -> Result<TimelineFact, TimelineError> {
    let timestamp = message.occurred_at_unix_ms;
    let draft = message.into_draft()?;
    let key = format!(
        "conversation-message:{}",
        draft.external_message_id.as_deref().unwrap()
    );
    let facts = repository.append_guarded(
        branch,
        expected_head,
        &key,
        json!({"source_time": timestamp}),
        vec![draft],
        guard,
    )?;
    facts.into_iter().next().ok_or(TimelineError::Invalid(
        "conversation message must commit one fact",
    ))
}

/// Share an existing conversation message with additional witnesses without
/// creating a second message or changing its original audience.
pub fn disclose_conversation_message(
    repository: &mut dyn TimelineRepository,
    branch: BranchId,
    expected_head: u64,
    guard: AppendGuard,
    disclosure_id: EventId,
    source: EventId,
    witnesses: BTreeSet<CharacterId>,
) -> Result<TimelineFact, TimelineError> {
    if witnesses.is_empty() {
        return Err(TimelineError::Invalid(
            "conversation disclosure requires witnesses",
        ));
    }
    let original = repository.event(branch, source)?;
    if !original.draft.kind.is_message() {
        return Err(TimelineError::Invalid(
            "conversation disclosure requires a message",
        ));
    }
    let mut draft = EventDraft::new(EventKind::EventObserved);
    draft.id = disclosure_id;
    draft.observers = witnesses;
    draft.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: source,
    });
    let facts = repository.append_guarded(
        branch,
        expected_head,
        &format!("conversation-disclosure:{}", disclosure_id.0),
        json!({}),
        vec![draft],
        guard,
    )?;
    facts.into_iter().next().ok_or(TimelineError::Invalid(
        "conversation disclosure must commit one fact",
    ))
}
