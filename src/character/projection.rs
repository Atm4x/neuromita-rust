use crate::{
    character::{
        CharacterState,
        state::{CharacterBehavior, CharacterWorkingState},
    },
    identity::CharacterId,
    memory::{CharacterMemories, MemoryChange},
    timeline::domain::{self, EventKind, TimelineError, TimelineFact},
};

pub(crate) fn character_state(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterState, TimelineError> {
    let count = domain::accepted_reply_count(facts, character)?;
    let behavior = character_behavior_at(facts, character)?;
    Ok(CharacterState {
        accepted_replies: count,
        revision: count,
        attitude: behavior.attitude,
        boredom: behavior.boredom,
        stress: behavior.stress,
    })
}

pub(crate) fn character_behavior_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterBehavior, TimelineError> {
    let mut current = CharacterBehavior::default();
    for fact in facts.iter() {
        if fact.draft.kind != EventKind::CharacterStateChanged
            || fact
                .draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                != Some(character)
        {
            continue;
        }
        current = serde_json::from_value(
            fact.draft
                .payload
                .get("state")
                .cloned()
                .ok_or(TimelineError::Invalid("state change has no state"))?,
        )
        .map_err(|_| TimelineError::Invalid("invalid state change"))?;
        if !current.is_valid() {
            return Err(TimelineError::Invalid("invalid accepted character state"));
        }
    }
    Ok(current)
}

pub(crate) fn character_working_state_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterWorkingState, TimelineError> {
    let mut current = CharacterWorkingState::default();
    for fact in facts.iter() {
        if fact.draft.kind == EventKind::CharacterContextReset
            && fact.draft.observers.contains(&character)
        {
            current = CharacterWorkingState::default();
        }
        if fact.draft.kind != EventKind::CharacterWorkingStateReplaced
            || fact
                .draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                != Some(character)
        {
            continue;
        }
        let next: CharacterWorkingState =
            serde_json::from_value(fact.draft.payload.get("working_state").cloned().ok_or(
                TimelineError::Invalid("working state effect has no payload"),
            )?)
            .map_err(|_| TimelineError::Invalid("invalid accepted working state"))?;
        if !next.is_valid() {
            return Err(TimelineError::Invalid("invalid accepted working state"));
        }
        current = next;
    }
    Ok(current)
}

/// Revision of this character's memory stream, independent of unrelated Timeline events.
/// The last committed event identity prevents an A -> B -> A edit from accepting
/// a stale snapshot whose visible records happen to match again.
pub(crate) fn character_memory_revision_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Option<crate::identity::EventId> {
    facts.iter().rev().find_map(|fact| {
        (fact.draft.kind == EventKind::CharacterMemoryChanged
            && fact.draft.actor.as_deref().and_then(CharacterId::parse) == Some(character))
        .then_some(fact.draft.id)
    })
}

pub(crate) fn character_memories_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<CharacterMemories, TimelineError> {
    let mut book = CharacterMemories::default();
    for fact in facts.iter() {
        if fact.draft.kind != EventKind::CharacterMemoryChanged
            || fact
                .draft
                .actor
                .as_deref()
                .and_then(crate::identity::CharacterId::parse)
                != Some(character)
        {
            continue;
        }
        let change: MemoryChange = serde_json::from_value(
            fact.draft
                .payload
                .get("change")
                .cloned()
                .ok_or(TimelineError::Invalid("missing memory change"))?,
        )
        .map_err(|_| TimelineError::Invalid("invalid committed memory change"))?;
        book.apply_committed(&change)
            .map_err(TimelineError::Invalid)?;
    }
    Ok(book)
}

pub(crate) fn validate_character_variables(
    vars: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), &'static str> {
    if vars.is_empty() || vars.len() > 64 {
        return Err("invalid character variables count");
    }
    for (name, value) in vars {
        if name.len() > 80
            || !name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            || matches!(
                name.as_str(),
                "CHARACTER_ID"
                    | "CURRENT_INPUT"
                    | "ACCEPTED_REPLIES"
                    | "attitude"
                    | "boredom"
                    | "stress"
            )
            || serde_json::to_vec(value).map_or(true, |data| data.len() > 4096)
        {
            return Err("invalid character variable value or name");
        }
    }
    Ok(())
}

pub(crate) fn character_variables_at(
    facts: &[TimelineFact],
    character: CharacterId,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, TimelineError> {
    let mut values = std::collections::BTreeMap::new();
    for fact in facts {
        if fact.draft.kind != EventKind::CharacterVariablesChanged
            || fact.draft.actor.as_deref().and_then(CharacterId::parse) != Some(character)
        {
            continue;
        }
        let changed: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_value(
                fact.draft
                    .payload
                    .get("variables")
                    .cloned()
                    .ok_or(TimelineError::Invalid("missing character variables"))?,
            )
            .map_err(|_| TimelineError::Invalid("invalid character variables"))?;
        validate_character_variables(&changed).map_err(TimelineError::Invalid)?;
        values.extend(changed);
    }
    Ok(values)
}
