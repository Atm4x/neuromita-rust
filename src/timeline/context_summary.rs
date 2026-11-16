use super::{
    ContextAnchor, SessionTimeline, TimelineSelection,
    domain::{CharacterMemories, CharacterWorkingState, TimelineError, TimelineFact},
};
use crate::{
    character::CharacterState,
    games::GameSlot,
    identity::{CharacterId, EventId},
};

const CONTEXT_RULES_VERSION: u32 = 1;

/// Identity of an inspected context. Each summary is specific to the observer
/// and cutoff, even when multiple branches share the same physical events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextReadVersion {
    pub anchor: ContextAnchor,
    pub character: CharacterId,
    pub branch_head: u64,
    pub knowledge_epoch: u64,
    pub selection: TimelineSelection,
    pub storage_revision: Option<u64>,
    pub rules: u32,
}

impl ContextReadVersion {
    pub(super) fn new(
        anchor: ContextAnchor,
        character: CharacterId,
        branch_head: u64,
        knowledge_epoch: u64,
        selection: TimelineSelection,
        storage_revision: Option<u64>,
    ) -> Self {
        Self {
            anchor,
            character,
            branch_head,
            knowledge_epoch,
            selection,
            storage_revision,
            rules: CONTEXT_RULES_VERSION,
        }
    }
}

#[derive(Clone)]
pub(super) struct ContextSummary {
    pub state: CharacterState,
    pub working_state: CharacterWorkingState,
    pub variables: std::collections::BTreeMap<String, serde_json::Value>,
    pub memories: CharacterMemories,
    pub game: GameSlot,
    pub game_token: Option<EventId>,
}

impl ContextSummary {
    pub(super) fn from_facts(
        character: CharacterId,
        at: ContextAnchor,
        facts: &[TimelineFact],
    ) -> Result<Self, TimelineError> {
        let replies = super::domain::accepted_reply_count(facts, character)?;
        Self::from_effects(character, at, facts, replies)
    }

    pub(super) fn from_effects(
        character: CharacterId,
        at: ContextAnchor,
        facts: &[TimelineFact],
        replies: u64,
    ) -> Result<Self, TimelineError> {
        use crate::character::projection;
        let (game, game_token) = SessionTimeline::game_from_facts(character, at, facts)
            .map_err(TimelineError::Storage)?;
        Ok(Self {
            state: projection::character_state_with_reply_count(facts, character, replies)?,
            working_state: projection::character_working_state_at(facts, character)?,
            variables: projection::character_variables_at(facts, character)?,
            memories: projection::character_memories_at(facts, character)?,
            game,
            game_token,
        })
    }
}

#[cfg(feature = "sqlite-system")]
pub(super) type IndexedContext = (
    ContextReadVersion,
    ContextSummary,
    Vec<super::domain::KnownEvent>,
);

/// Discardable in-process state summaries. The observer's knowledge and
/// messages are always read from the canonical repository.
#[derive(Default)]
pub(super) struct ContextSummaryCache {
    by_character: std::collections::HashMap<CharacterId, (ContextReadVersion, ContextSummary)>,
}

impl ContextSummaryCache {
    pub(super) fn get_or_project(
        &mut self,
        version: ContextReadVersion,
        project: impl FnOnce() -> Result<ContextSummary, TimelineError>,
    ) -> Result<ContextSummary, TimelineError> {
        if let Some((cached_version, summary)) = self.by_character.get(&version.character)
            && *cached_version == version
        {
            return Ok(summary.clone());
        }
        let summary = project()?;
        self.by_character
            .insert(version.character, (version, summary.clone()));
        Ok(summary)
    }
}
