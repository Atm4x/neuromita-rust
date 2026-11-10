use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{
    ContextAnchor, SessionTimeline,
    domain::{AppendGuard, EventDraft, EventKind, TimelineError},
    selection::TimelineSelection,
};
use crate::identity::CharacterId;

impl SessionTimeline {
    pub(crate) fn append_character_variable_changes(
        &mut self,
        character: CharacterId,
        selection: TimelineSelection,
        anchor: ContextAnchor,
        context_epoch: u64,
        changed: &BTreeMap<String, Value>,
    ) -> Result<(), TimelineError> {
        crate::character::projection::validate_character_variables(changed)
            .map_err(TimelineError::Invalid)?;
        self.assert_selection(selection)?;
        if self.anchor() != anchor
            || self.admission(character, selection)?.context_epoch != context_epoch
        {
            return Err(TimelineError::ConcurrentWrite);
        }
        let mut draft = EventDraft::new(EventKind::CharacterVariablesChanged);
        draft.actor = Some(character.to_string());
        draft.observers.insert(character);
        draft.payload.insert(
            "variables".into(),
            serde_json::to_value(changed)
                .map_err(|_| TimelineError::Invalid("variable serialization failed"))?,
        );
        self.store.append_guarded(
            self.branch,
            anchor.through,
            &format!(
                "character-variable-import:{}:{}",
                character,
                uuid::Uuid::new_v4()
            ),
            json!({"character": character.to_string(), "expected_head": anchor.through}),
            vec![draft],
            AppendGuard {
                selection_generation: selection.generation,
                context: Some((character, context_epoch, None)),
                accepted_reply_revision: None,
            },
        )?;
        Ok(())
    }
}
