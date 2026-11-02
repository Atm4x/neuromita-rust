use serde_json::json;

use super::{
    ContextAnchor, SessionTimeline,
    domain::{AppendGuard, EventDraft, EventKind, TimelineError},
    selection::TimelineSelection,
};
use crate::{
    character::memory_access::CharacterMemorySnapshot,
    identity::CharacterId,
    memory::{MemoryChange, MemoryCommand},
};

impl SessionTimeline {
    pub(crate) fn character_memory_snapshot(
        &self,
        character: CharacterId,
    ) -> Result<CharacterMemorySnapshot, TimelineError> {
        let selection = self.selection();
        self.assert_selection(selection)?;
        let anchor = self.anchor();
        let world = self.store.world(self.branch)?;
        let records = crate::character::projection::character_memories_at(&world, character)?.all();
        Ok(CharacterMemorySnapshot {
            character,
            anchor,
            records,
            selection,
        })
    }

    /// A user edit may only soft-delete. Historical hard deletion is reserved for
    /// the separate backup-first administrative maintenance workflow.
    pub(crate) fn edit_character_memories(
        &mut self,
        character: CharacterId,
        expected_selection: TimelineSelection,
        expected_anchor: ContextAnchor,
        commands: &[MemoryCommand],
    ) -> Result<Vec<MemoryChange>, TimelineError> {
        self.assert_selection(expected_selection)?;
        if self.anchor() != expected_anchor {
            return Err(TimelineError::ConcurrentWrite);
        }
        if commands.is_empty() || commands.len() > 128 {
            return Err(TimelineError::Invalid(
                "memory edit requires 1..128 operations",
            ));
        }
        if commands
            .iter()
            .any(|cmd| matches!(cmd, MemoryCommand::Delete { .. }))
        {
            return Err(TimelineError::Invalid("hard deletion is not a user edit"));
        }
        let world = self.store.world(self.branch)?;
        let memories = crate::character::projection::character_memories_at(&world, character)?;
        let changes = memories.resolve(commands).map_err(TimelineError::Invalid)?;
        if changes.is_empty() {
            return Ok(changes);
        }
        let mut drafts = Vec::with_capacity(changes.len());
        for change in &changes {
            let mut event = EventDraft::new(EventKind::CharacterMemoryChanged);
            event.actor = Some(character.to_string());
            event.observers.insert(character);
            event.payload.insert(
                "change".into(),
                serde_json::to_value(change)
                    .map_err(|_| TimelineError::Invalid("memory change serialization failed"))?,
            );
            drafts.push(event);
        }
        self.store.append_guarded(
            self.branch,
            expected_anchor.through,
            &format!(
                "character-memory-edit:{}:{}",
                character,
                uuid::Uuid::new_v4()
            ),
            json!({"character":character.to_string(), "expected_head":expected_anchor.through}),
            drafts,
            AppendGuard {
                selection_generation: expected_selection.generation,
                context: None,
                accepted_reply_revision: None,
            },
        )?;
        Ok(changes)
    }
}
