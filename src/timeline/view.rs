use serde_json::{Map, Value};

use crate::identity::CharacterId;

use super::domain::{
    BranchId, CausalRelation, CharacterMessageProjection, EventKind, KnownEvent, MessageRole,
    ProjectedMessage, TimelineError, TimelineRepository,
};

fn provider_document(message: ProjectedMessage) -> Map<String, Value> {
    let mut document = message.fields;
    document.insert("message_id".into(), Value::String(message.message_id));
    document.insert(
        "role".into(),
        Value::String(
            match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::System => "system",
            }
            .into(),
        ),
    );
    document.insert(
        "speaker".into(),
        message
            .speaker
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    document.insert(
        "sender".into(),
        message.speaker.map(Value::String).unwrap_or(Value::Null),
    );
    document
        .entry("content")
        .or_insert(Value::String(String::new()));
    document.insert(
        "timeline_event_id".into(),
        Value::String(message.event_id.0.to_string()),
    );
    document.insert("timeline_sequence".into(), Value::from(message.sequence));
    document.insert(
        "timeline_learned_at".into(),
        Value::from(message.learned_at),
    );
    document.insert("_history_row_id".into(), Value::from(message.learned_at));
    document
}

/// Explicit prompt inclusion policy, separate from a character's knowledge.
/// Later world notes and observations may be relevant to a reaction, while later
/// unrelated dialogue must not silently become part of its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptHistoryPolicy {
    AllVisible,
    CauseScoped { cause_sequence: u64 },
}

impl PromptHistoryPolicy {
    fn includes(self, message: &ProjectedMessage) -> bool {
        match self {
            Self::AllVisible => true,
            Self::CauseScoped { cause_sequence } => {
                message.sequence <= cause_sequence
                    || !matches!(
                        message.kind,
                        EventKind::PlayerMessage | EventKind::CharacterMessage
                    )
            }
        }
    }
}

/// Detached view of a fixed branch prefix; no provider state or mutable character
/// state is stored in the canonical timeline.
#[derive(Debug, Clone)]
pub struct MitaView {
    pub branch: BranchId,
    pub through: u64,
    projection: CharacterMessageProjection,
}

impl MitaView {
    pub fn count(&self) -> usize {
        self.projection.count()
    }

    pub fn contains(&self, message_id: &str) -> bool {
        self.projection.contains(message_id)
    }

    pub fn messages(&self) -> Vec<ProjectedMessage> {
        self.projection.messages()
    }

    pub fn prompt_messages(&self, policy: PromptHistoryPolicy) -> Vec<ProjectedMessage> {
        self.messages()
            .into_iter()
            .filter(|message| policy.includes(message))
            .collect()
    }

    pub fn provider_messages(&self) -> Vec<Map<String, Value>> {
        self.messages().into_iter().map(provider_document).collect()
    }

    /// The most recent `limit` visible messages, skipping `offset` from the tail.
    pub fn page(&self, limit: usize, offset: usize) -> Vec<Map<String, Value>> {
        self.projection
            .page(limit, offset)
            .into_iter()
            .map(provider_document)
            .collect()
    }
}

pub struct TimelineMessageQueries;

impl TimelineMessageQueries {
    /// Scans direct witnesses through a fixed head in bounded repository pages.
    /// Disclosure resolves original facts without exposing undisclosed information.
    pub fn capture(
        repository: &dyn TimelineRepository,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<MitaView, TimelineError> {
        let info = repository.branch(branch)?;
        let head = through.unwrap_or(info.head_sequence);
        if head > info.head_sequence {
            return Err(TimelineError::Invalid(
                "context position exceeds branch head",
            ));
        }
        let mut projection = CharacterMessageProjection::new(character);
        let mut cursor = 0_u64;
        loop {
            let page = repository.read(branch, cursor, Some(head), 128, Some(character))?;
            if page.is_empty() {
                break;
            }
            for fact in &page {
                if fact.draft.kind == EventKind::EventObserved {
                    for link in &fact.draft.links {
                        if link.relation == CausalRelation::Observes {
                            let original = repository.event(branch, link.event_id)?;
                            if original.sequence >= fact.sequence {
                                return Err(TimelineError::Invalid(
                                    "disclosure target is not in past",
                                ));
                            }
                            projection.observe(&KnownEvent {
                                fact: original,
                                learned_at: fact.sequence,
                            });
                        }
                    }
                }
                projection.observe(&KnownEvent {
                    fact: fact.clone(),
                    learned_at: fact.sequence,
                });
            }
            cursor = page.last().expect("nonempty page").sequence;
        }
        Ok(MitaView {
            branch,
            through: head,
            projection,
        })
    }
}
