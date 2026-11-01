use std::collections::{HashMap, HashSet};

use crate::identity::{CharacterId, EventId};

use super::model::{CausalRelation, EventKind, TimelineFact};

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
    base: &'a WorldIndex,
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

impl<'a> AppendIndex<'a> {
    pub(super) fn new(base: &'a WorldIndex) -> Self {
        Self {
            base,
            kinds: HashMap::new(),
            external_ids: HashSet::new(),
            knowledge: HashMap::new(),
        }
    }

    pub(super) fn kind(&self, event: EventId) -> Option<EventKind> {
        self.kinds
            .get(&event)
            .or_else(|| self.base.kinds.get(&event))
            .copied()
    }

    pub(super) fn has_external(&self, external: &str) -> bool {
        self.external_ids.contains(external) || self.base.external_ids.contains(external)
    }

    pub(super) fn knows(&self, character: CharacterId, event: EventId) -> bool {
        if let Some(change) = self.knowledge.get(&character) {
            if change.added.contains(&event) {
                return true;
            }
            if change.reset {
                return false;
            }
        }
        self.base
            .known
            .get(&character)
            .is_some_and(|events| events.contains(&event))
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
