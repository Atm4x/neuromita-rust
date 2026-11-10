use crate::identity::{CharacterId, EventId};
use serde_json::Value;

use super::model::{EventKind, TimelineFact};

#[derive(Debug, Clone)]
pub(crate) struct KnowledgeChange {
    pub reset: bool,
    pub acquired: EventId,
    pub forgotten: Vec<EventId>,
    pub revealed: Vec<EventId>,
}

pub(crate) fn knowledge_targets(payload: &serde_json::Map<String, Value>) -> Option<Vec<EventId>> {
    let ids = payload.get("event_ids")?.as_array()?;
    ids.iter()
        .map(|value| Some(EventId(uuid::Uuid::parse_str(value.as_str()?).ok()?)))
        .collect()
}

/// Direct witnesses acquire exactly this event. Privileged knowledge-control
/// events independently revoke/grant the specifically referenced historical IDs.
/// Causal links or a story never imply a knowledge transfer.
pub(crate) fn knowledge_change(
    fact: &TimelineFact,
    observer: CharacterId,
) -> Option<KnowledgeChange> {
    if fact.draft.kind == EventKind::GameStateCaptured {
        return None;
    }
    if !fact.draft.observers.contains(&observer) {
        return None;
    }
    let targets = knowledge_targets(&fact.draft.payload).unwrap_or_default();
    Some(KnowledgeChange {
        reset: fact.draft.kind == EventKind::CharacterContextReset,
        acquired: fact.draft.id,
        forgotten: if fact.draft.kind == EventKind::CharacterKnowledgeForgotten {
            targets.clone()
        } else {
            Vec::new()
        },
        revealed: if fact.draft.kind == EventKind::CharacterKnowledgeRevealed {
            targets
        } else {
            Vec::new()
        },
    })
}
