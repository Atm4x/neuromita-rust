use crate::{
    identity::CharacterId,
    memory::{MemoryChange, MemoryRecord},
    timeline::{ContextAnchor, selection::TimelineSelection},
};

/// A presenter-facing, immutable view of one character's retained knowledge.
/// Its private selection token prevents edits after switching Timeline branches.
#[derive(Debug, Clone)]
pub struct CharacterMemorySnapshot {
    pub(crate) character: CharacterId,
    pub(crate) anchor: ContextAnchor,
    pub(crate) records: Vec<MemoryRecord>,
    pub(crate) selection: TimelineSelection,
}

impl CharacterMemorySnapshot {
    pub fn character(&self) -> CharacterId {
        self.character
    }

    pub fn anchor(&self) -> ContextAnchor {
        self.anchor
    }

    pub fn records(&self) -> &[MemoryRecord] {
        &self.records
    }
}

#[derive(Debug, Clone)]
pub struct MemoryEditResult {
    pub applied: Vec<MemoryChange>,
    pub snapshot: CharacterMemorySnapshot,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio_util::sync::CancellationToken;

    use crate::{
        character::{CharacterError, CharacterId, CharacterRegistry},
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryState},
        timeline::SessionTimeline,
    };

    #[tokio::test]
    async fn memory_edits_are_character_owned_versioned_and_soft_delete_only() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Mita).unwrap();
        let kind = registry.get(CharacterId::KindMita).unwrap();
        let before = mita.memories().await.unwrap();
        assert!(before.records.is_empty());
        let added = mita
            .edit_memories(
                before.clone(),
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "A remembered detail".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(added.applied.len(), 1);
        let record = &added.snapshot.records[0];
        let id = record.id;
        assert_eq!(record.content, "A remembered detail");
        assert!(kind.memories().await.unwrap().records.is_empty());
        assert!(matches!(
            mita.edit_memories(before, vec![MemoryCommand::Forget { id }])
                .await,
            Err(CharacterError::StaleState)
        ));
        assert!(matches!(
            kind.edit_memories(added.snapshot.clone(), vec![MemoryCommand::Forget { id }])
                .await,
            Err(CharacterError::StaleState)
        ));
        assert!(
            mita.edit_memories(added.snapshot.clone(), vec![MemoryCommand::Delete { id }])
                .await
                .is_err()
        );
        assert_eq!(mita.memories().await.unwrap().records.len(), 1);
        let secured = mita
            .edit_memories(
                added.snapshot,
                vec![MemoryCommand::SetProtection {
                    id,
                    protected: true,
                }],
            )
            .await
            .unwrap();
        assert!(
            mita.edit_memories(
                secured.snapshot.clone(),
                vec![MemoryCommand::SoftDelete { id }]
            )
            .await
            .is_err()
        );
        let unlocked = mita
            .edit_memories(
                secured.snapshot,
                vec![MemoryCommand::SetProtection {
                    id,
                    protected: false,
                }],
            )
            .await
            .unwrap();
        let deleted = mita
            .edit_memories(unlocked.snapshot, vec![MemoryCommand::SoftDelete { id }])
            .await
            .unwrap();
        assert_eq!(deleted.snapshot.records[0].state, MemoryState::Deleted);
        assert_eq!(deleted.snapshot.records[0].id, id);
        assert_eq!(kind.memories().await.unwrap().records.len(), 0);
        let invalid = mita
            .edit_memories(
                deleted.snapshot,
                vec![MemoryCommand::Restore {
                    id: MemoryId::new(),
                }],
            )
            .await;
        assert!(invalid.is_err());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }
}
