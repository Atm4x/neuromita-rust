use std::collections::{HashMap, HashSet};

use crate::identity::{CharacterId, EventId};

use super::model::TimelineError;
use super::model::{CausalRelation, EventKind, TimelineFact};

/// Read-only facts needed by the single domain append validator.
/// A repository can answer these with indexed storage reads without reconstructing
/// a full world. No storage-specific rule is encoded in this contract.
pub(crate) trait WorldLookup {
    fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError>;
    fn has_external(&self, external: &str) -> Result<bool, TimelineError>;
    fn knows(&self, character: CharacterId, event: EventId) -> Result<bool, TimelineError>;
}

#[derive(Debug, Clone, Default)]
pub(super) struct WorldIndex {
    kinds: HashMap<EventId, EventKind>,
    external_ids: HashSet<String>,
    known: HashMap<CharacterId, HashSet<EventId>>,
}

#[derive(Debug, Clone, Default)]
struct KnowledgeChange {
    reset: bool,
    added: HashSet<EventId>,
}

pub(super) struct AppendIndex<'a> {
    base: &'a dyn WorldLookup,
    kinds: HashMap<EventId, EventKind>,
    external_ids: HashSet<String>,
    knowledge: HashMap<CharacterId, KnowledgeChange>,
}

impl WorldIndex {
    pub(super) fn from_facts(facts: &[TimelineFact]) -> Self {
        let mut index = Self::default();
        for fact in facts {
            index.insert(fact);
        }
        index
    }

    fn insert(&mut self, fact: &TimelineFact) {
        self.kinds.insert(fact.draft.id, fact.draft.kind);
        if let Some(external) = &fact.draft.external_message_id {
            self.external_ids.insert(external.clone());
        }
        for observer in &fact.draft.observers {
            let known = self.known.entry(*observer).or_default();
            if fact.draft.kind == EventKind::CharacterContextReset {
                known.clear();
            }
            known.insert(fact.draft.id);
            if fact.draft.kind == EventKind::EventObserved {
                for link in &fact.draft.links {
                    if link.relation == CausalRelation::Observes {
                        known.insert(link.event_id);
                    }
                }
            }
        }
    }

    pub(super) fn extend(&mut self, facts: &[TimelineFact]) {
        for fact in facts {
            self.insert(fact);
        }
    }
}

impl WorldLookup for WorldIndex {
    fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError> {
        Ok(self.kinds.get(&event).copied())
    }

    fn has_external(&self, external: &str) -> Result<bool, TimelineError> {
        Ok(self.external_ids.contains(external))
    }

    fn knows(&self, character: CharacterId, event: EventId) -> Result<bool, TimelineError> {
        Ok(self
            .known
            .get(&character)
            .is_some_and(|ids| ids.contains(&event)))
    }
}

impl<'a> AppendIndex<'a> {
    pub(super) fn new(base: &'a dyn WorldLookup) -> Self {
        Self {
            base,
            kinds: HashMap::new(),
            external_ids: HashSet::new(),
            knowledge: HashMap::new(),
        }
    }

    pub(super) fn kind(&self, event: EventId) -> Result<Option<EventKind>, TimelineError> {
        if let Some(kind) = self.kinds.get(&event) {
            Ok(Some(*kind))
        } else {
            self.base.kind(event)
        }
    }

    pub(super) fn has_external(&self, external: &str) -> Result<bool, TimelineError> {
        if self.external_ids.contains(external) {
            Ok(true)
        } else {
            self.base.has_external(external)
        }
    }

    pub(super) fn knows(
        &self,
        character: CharacterId,
        event: EventId,
    ) -> Result<bool, TimelineError> {
        if let Some(change) = self.knowledge.get(&character) {
            if change.added.contains(&event) {
                return Ok(true);
            }
            if change.reset {
                return Ok(false);
            }
        }
        self.base.knows(character, event)
    }

    pub(super) fn record(&mut self, fact: &TimelineFact) {
        self.kinds.insert(fact.draft.id, fact.draft.kind);
        if let Some(external) = &fact.draft.external_message_id {
            self.external_ids.insert(external.clone());
        }
        for observer in &fact.draft.observers {
            let change = self.knowledge.entry(*observer).or_default();
            if fact.draft.kind == EventKind::CharacterContextReset {
                change.reset = true;
                change.added.clear();
            }
            change.added.insert(fact.draft.id);
            if fact.draft.kind == EventKind::EventObserved {
                for link in &fact.draft.links {
                    if link.relation == CausalRelation::Observes {
                        change.added.insert(link.event_id);
                    }
                }
            }
        }
    }
}
