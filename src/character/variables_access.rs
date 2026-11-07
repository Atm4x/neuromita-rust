use std::collections::BTreeMap;

use serde_json::Value;

use crate::{
    identity::CharacterId,
    timeline::{
        ContextAnchor, SessionTimeline, domain::TimelineError, selection::TimelineSelection,
    },
};

#[derive(Debug, Clone)]
pub struct CharacterVariablesSnapshot {
    character: CharacterId,
    anchor: ContextAnchor,
    values: BTreeMap<String, Value>,
    selection: TimelineSelection,
    context_epoch: u64,
}

impl CharacterVariablesSnapshot {
    pub fn character(&self) -> CharacterId {
        self.character
    }

    pub fn anchor(&self) -> ContextAnchor {
        self.anchor
    }

    pub fn values(&self) -> &BTreeMap<String, Value> {
        &self.values
    }
}

pub(crate) fn read_variables_snapshot(
    timeline: &SessionTimeline,
    character: CharacterId,
) -> Result<CharacterVariablesSnapshot, TimelineError> {
    let (selection, anchor, world) = timeline.selected_world_snapshot()?;
    let values = super::projection::character_variables_at(&world, character)?;
    let context_epoch = timeline.admission(character, selection)?.context_epoch;
    Ok(CharacterVariablesSnapshot {
        character,
        anchor,
        values,
        selection,
        context_epoch,
    })
}

pub(crate) fn edit_variables_snapshot(
    timeline: &mut SessionTimeline,
    character: CharacterId,
    expected: &CharacterVariablesSnapshot,
    incoming: &BTreeMap<String, Value>,
) -> Result<CharacterVariablesSnapshot, TimelineError> {
    if expected.character != character {
        return Err(TimelineError::ConcurrentWrite);
    }
    if !incoming.is_empty() {
        super::projection::validate_character_variables(incoming)
            .map_err(TimelineError::Invalid)?;
    }
    let current = read_variables_snapshot(timeline, character)?;
    if current.selection != expected.selection
        || current.anchor != expected.anchor
        || current.context_epoch != expected.context_epoch
        || current.values != expected.values
    {
        return Err(TimelineError::ConcurrentWrite);
    }
    let mut merged = current.values;
    let mut added = BTreeMap::new();
    for (name, value) in incoming {
        match merged.get(name) {
            Some(existing) if existing == value => {}
            Some(_) => return Err(TimelineError::Invalid("variable identity conflict")),
            None => {
                merged.insert(name.clone(), value.clone());
                added.insert(name.clone(), value.clone());
            }
        }
    }
    if !merged.is_empty() {
        super::projection::validate_character_variables(&merged).map_err(TimelineError::Invalid)?;
    }
    if !added.is_empty() {
        timeline.append_character_variable_changes(
            character,
            expected.selection,
            expected.anchor,
            expected.context_epoch,
            &added,
        )?;
    }
    read_variables_snapshot(timeline, character)
}
