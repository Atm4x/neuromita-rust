use crate::identity::{CharacterId, EventId};

use super::model::{CausalRelation, EventKind, TimelineFact};

/// Canonical knowledge transition derived from a single immutable timeline fact.
/// Storage indexes and read projections consume the same transition.
pub(crate) struct KnowledgeChange {
    pub reset: bool,
    pub acquired: Vec<EventId>,
}

pub(crate) fn knowledge_change(
    fact: &TimelineFact,
    observer: CharacterId,
) -> Option<KnowledgeChange> {
    if !fact.draft.observers.contains(&observer) {
        return None;
    }
    let reset = fact.draft.kind == EventKind::CharacterContextReset;
    let mut acquired = vec![fact.draft.id];
    if fact.draft.kind == EventKind::EventObserved {
        acquired.extend(
            fact.draft
                .links
                .iter()
                .filter(|link| link.relation == CausalRelation::Observes)
                .map(|link| link.event_id),
        );
    }
    Some(KnowledgeChange { reset, acquired })
}
