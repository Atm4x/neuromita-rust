use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    ContextAnchor, SessionTimeline,
    domain::{AppendGuard, EventDraft, EventKind, TimelineError},
    selection::TimelineSelection,
};
use crate::{
    identity::CharacterId,
    memory::{MAX_MEMORY_EDIT_CHANGES, MemoryChange},
};

impl SessionTimeline {
    /// One guarded repository append commits the memory and variable facts together.
    /// Timeline never owns a mutable copy of either character projection.
    pub(crate) fn append_character_portable_changes(
        &mut self,
        character: CharacterId,
        selection: TimelineSelection,
        anchor: ContextAnchor,
        context_epoch: u64,
        memories: &[MemoryChange],
        variables: &BTreeMap<String, Value>,
    ) -> Result<(), TimelineError> {
        if (memories.is_empty() && variables.is_empty())
            || memories.len() > MAX_MEMORY_EDIT_CHANGES
            || memories
                .iter()
                .any(|change| !matches!(change, MemoryChange::Upsert(_)))
        {
            return Err(TimelineError::Invalid(
                "portable import requires 1..256 exact upserts or variables",
            ));
        }
        if !variables.is_empty() {
            crate::character::projection::validate_character_variables(variables)
                .map_err(TimelineError::Invalid)?;
        }
        self.assert_selection(selection)?;
        if self.anchor() != anchor
            || self.admission(character, selection)?.context_epoch != context_epoch
        {
            return Err(TimelineError::ConcurrentWrite);
        }
        let mut drafts = Vec::with_capacity(memories.len() + usize::from(!variables.is_empty()));
        for change in memories {
            let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
            draft.actor = Some(character.to_string());
            draft.observers.insert(character);
            draft.payload.insert(
                "change".into(),
                serde_json::to_value(change)
                    .map_err(|_| TimelineError::Invalid("memory serialization failed"))?,
            );
            drafts.push(draft);
        }
        if !variables.is_empty() {
            let mut draft = EventDraft::new(EventKind::CharacterVariablesChanged);
            draft.actor = Some(character.to_string());
            draft.observers.insert(character);
            draft.payload.insert(
                "variables".into(),
                serde_json::to_value(variables)
                    .map_err(|_| TimelineError::Invalid("variable serialization failed"))?,
            );
            drafts.push(draft);
        }
        self.store.append_guarded(
            self.branch,
            anchor.through,
            &format!(
                "character-portable-import:{character}:{}",
                uuid::Uuid::new_v4()
            ),
            json!({"character": character.to_string(), "expected_head": anchor.through}),
            drafts,
            AppendGuard {
                selection_generation: selection.generation,
                context: Some((character, context_epoch, None)),
                accepted_reply_revision: None,
            },
        )?;
        if !memories.is_empty() {
            self.notify_memory_changed(character);
        }
        Ok(())
    }
}
