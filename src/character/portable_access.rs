use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    memory_access::{CharacterMemorySnapshot, read_memory_snapshot},
    variables_access::{CharacterVariablesSnapshot, read_variables_snapshot},
};
use crate::{
    identity::CharacterId,
    memory::{MAX_MEMORY_EDIT_CHANGES, MemoryCommand},
    timeline::{SessionTimeline, domain::TimelineError},
};

#[derive(Debug, Clone)]
pub struct CharacterPortableSnapshot {
    pub memory: CharacterMemorySnapshot,
    pub variables: CharacterVariablesSnapshot,
}

#[derive(Debug, Clone)]
pub struct CharacterPortableResult {
    pub applied_memories: usize,
    pub applied_variables: usize,
    pub snapshot: CharacterPortableSnapshot,
}

pub(crate) fn read_portable_snapshot(
    timeline: &SessionTimeline,
    character: CharacterId,
) -> Result<CharacterPortableSnapshot, TimelineError> {
    let memory = read_memory_snapshot(timeline, character)?;
    let variables = read_variables_snapshot(timeline, character)?;
    if memory.anchor() != variables.anchor() {
        return Err(TimelineError::ConcurrentWrite);
    }
    Ok(CharacterPortableSnapshot { memory, variables })
}

pub(crate) fn import_portable_snapshot(
    timeline: &mut SessionTimeline,
    character: CharacterId,
    expected: &CharacterPortableSnapshot,
    memory_commands: &[MemoryCommand],
    variable_additions: &BTreeMap<String, Value>,
) -> Result<CharacterPortableResult, TimelineError> {
    if expected.memory.character() != character
        || expected.variables.character() != character
        || expected.memory.anchor() != expected.variables.anchor()
        || memory_commands.len() > MAX_MEMORY_EDIT_CHANGES
        || memory_commands
            .iter()
            .any(|command| !matches!(command, MemoryCommand::ImportRecord { .. }))
    {
        return Err(TimelineError::Invalid(
            "portable import requires one character and exact identity inserts",
        ));
    }
    let current = read_portable_snapshot(timeline, character)?;
    if !expected.memory.matches_version(&current.memory)
        || expected.memory.anchor() != current.memory.anchor()
        || expected.variables.anchor() != current.variables.anchor()
        || expected.variables.values() != current.variables.values()
    {
        return Err(TimelineError::ConcurrentWrite);
    }
    let (_, _, world) = timeline.selected_world_snapshot()?;
    let projection = super::projection::character_memories_at(&world, character)?;
    let memory_changes = projection
        .resolve(memory_commands)
        .map_err(TimelineError::Invalid)?;
    let mut merged = current.variables.values().clone();
    let mut additions = BTreeMap::new();
    for (key, value) in variable_additions {
        match merged.get(key) {
            Some(existing) if existing == value => {}
            Some(_) => return Err(TimelineError::Invalid("variable identity conflict")),
            None => {
                merged.insert(key.clone(), value.clone());
                additions.insert(key.clone(), value.clone());
            }
        }
    }
    if !merged.is_empty() {
        super::projection::validate_character_variables(&merged).map_err(TimelineError::Invalid)?;
    }
    if !memory_changes.is_empty() || !additions.is_empty() {
        timeline.append_character_portable_changes(
            character,
            current.memory.selection,
            current.memory.anchor(),
            current.memory.context_epoch,
            &memory_changes,
            &additions,
        )?;
    }
    Ok(CharacterPortableResult {
        applied_memories: memory_changes.len(),
        applied_variables: additions.len(),
        snapshot: read_portable_snapshot(timeline, character)?,
    })
}
