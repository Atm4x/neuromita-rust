use std::collections::BTreeMap;

use crate::{
    identity::{CharacterId, EventId},
    memory::{
        MAX_MEMORY_EDIT_CHANGES, MemoryChange, MemoryCommand, MemoryRecord, MemoryRelation,
        ProvenancedRelation,
    },
    timeline::{
        ContextAnchor, SessionTimeline, domain::TimelineError, selection::TimelineSelection,
    },
};

/// A presenter-facing, immutable view of one character's retained knowledge.
/// Its private selection token prevents edits after switching Timeline branches.
#[derive(Debug, Clone)]
pub struct CharacterMemorySnapshot {
    pub(crate) character: CharacterId,
    pub(crate) anchor: ContextAnchor,
    pub(crate) records: Vec<MemoryRecord>,
    pub(crate) relations: Vec<MemoryRelation>,
    pub(crate) extracted_relations: Vec<ProvenancedRelation>,
    pub(crate) memory_revision: Option<EventId>,
    pub(crate) context_epoch: u64,
    pub(crate) source_revisions: BTreeMap<crate::memory::MemoryId, u64>,
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

    pub fn relations(&self) -> &[MemoryRelation] {
        &self.relations
    }

    pub fn source_revision(&self, source: crate::memory::MemoryId) -> Option<u64> {
        self.source_revisions.get(&source).copied()
    }

    pub fn extracted_relations(&self) -> &[ProvenancedRelation] {
        &self.extracted_relations
    }

    /// Branch selection, character memory revision and context-reset epoch,
    /// without treating unrelated Timeline appends as memory-edit conflicts.
    pub(crate) fn matches_version(&self, other: &Self) -> bool {
        self.character == other.character
            && self.selection == other.selection
            && self.anchor.branch_id == other.anchor.branch_id
            && self.records == other.records
            && self.relations == other.relations
            && self.extracted_relations == other.extracted_relations
            && self.memory_revision == other.memory_revision
            && self.context_epoch == other.context_epoch
            && self.source_revisions == other.source_revisions
    }
}

#[derive(Debug, Clone)]
pub struct MemoryEditResult {
    pub applied: Vec<MemoryChange>,
    pub snapshot: CharacterMemorySnapshot,
}

/// Construct the character-owned projection from the authoritative Timeline snapshot.
/// The Timeline module itself must not know about presenter-facing character DTOs.
pub(crate) fn read_memory_snapshot(
    timeline: &SessionTimeline,
    character: CharacterId,
) -> Result<CharacterMemorySnapshot, TimelineError> {
    let (selection, anchor, world) = timeline.selected_world_snapshot()?;
    let projection = super::projection::character_memories_at(&world, character)?;
    let records = projection.all();
    let relations = projection.relations();
    let extracted_relations = projection.extracted_relations_at(0);
    let memory_revision = super::projection::character_memory_revision_at(&world, character);
    let context_epoch = timeline.admission(character, selection)?.context_epoch;
    let source_revisions = projection.source_revisions();
    Ok(CharacterMemorySnapshot {
        character,
        anchor,
        records,
        relations,
        extracted_relations,
        memory_revision,
        context_epoch,
        source_revisions,
        selection,
    })
}

/// Resolve the command inside the character boundary, then atomically persist
/// the derived changes with Timeline's existing guarded append operation.
pub(crate) fn edit_memory_snapshot(
    timeline: &mut SessionTimeline,
    character: CharacterId,
    expected: &CharacterMemorySnapshot,
    commands: &[MemoryCommand],
) -> Result<MemoryEditResult, TimelineError> {
    if expected.character != character {
        return Err(TimelineError::ConcurrentWrite);
    }
    if commands.is_empty() || commands.len() > MAX_MEMORY_EDIT_CHANGES {
        return Err(TimelineError::Invalid(
            "memory edit requires 1..256 operations",
        ));
    }
    if commands
        .iter()
        .any(|command| matches!(command, MemoryCommand::Delete { .. }))
    {
        return Err(TimelineError::Invalid("hard deletion is not a user edit"));
    }
    let (selection, anchor, world) = timeline.selected_world_snapshot()?;
    if selection != expected.selection
        || anchor.branch_id != expected.anchor.branch_id
        || timeline.admission(character, selection)?.context_epoch != expected.context_epoch
    {
        return Err(TimelineError::ConcurrentWrite);
    }
    let memories = super::projection::character_memories_at(&world, character)?;
    if memories.all() != expected.records
        || memories.relations() != expected.relations
        || memories.extracted_relations_at(0) != expected.extracted_relations
        || super::projection::character_memory_revision_at(&world, character)
            != expected.memory_revision
        || memories.source_revisions() != expected.source_revisions
    {
        return Err(TimelineError::ConcurrentWrite);
    }
    let changes = memories.resolve(commands).map_err(TimelineError::Invalid)?;
    if !changes.is_empty() {
        timeline.append_character_memory_changes(
            character,
            selection,
            anchor,
            expected.context_epoch,
            &changes,
        )?;
    }
    Ok(MemoryEditResult {
        applied: changes,
        snapshot: read_memory_snapshot(timeline, character)?,
    })
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
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let kind = registry.get(CharacterId::Kind).unwrap();
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

    #[test]
    fn context_reset_invalidates_only_the_affected_character_memory_editor() {
        use crate::{identity::EventId, timeline::domain::TimelineError};

        let mut timeline = SessionTimeline::new();
        let initial = super::read_memory_snapshot(&timeline, CharacterId::Crazy).unwrap();
        let other = super::read_memory_snapshot(&timeline, CharacterId::Kind).unwrap();
        let result = super::edit_memory_snapshot(
            &mut timeline,
            CharacterId::Crazy,
            &initial,
            &[MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "Retained across context reset".into(),
            }],
        )
        .unwrap();
        let stale = result.snapshot;
        let id = stale.records()[0].id;
        timeline
            .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
            .unwrap();
        let fresh = super::read_memory_snapshot(&timeline, CharacterId::Crazy).unwrap();
        assert_eq!(stale.records(), fresh.records());
        assert_eq!(stale.memory_revision, fresh.memory_revision);
        assert_ne!(stale.context_epoch, fresh.context_epoch);
        assert_eq!(
            super::edit_memory_snapshot(
                &mut timeline,
                CharacterId::Crazy,
                &stale,
                &[MemoryCommand::Forget { id }],
            )
            .err(),
            Some(TimelineError::ConcurrentWrite),
        );
        assert_eq!(
            super::read_memory_snapshot(&timeline, CharacterId::Crazy)
                .unwrap()
                .records()[0]
                .state,
            MemoryState::Active,
        );
        let unaffected = super::edit_memory_snapshot(
            &mut timeline,
            CharacterId::Kind,
            &other,
            &[MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "Independent".into(),
            }],
        )
        .unwrap();
        assert_eq!(unaffected.snapshot.records().len(), 1);
        let applied = super::edit_memory_snapshot(
            &mut timeline,
            CharacterId::Crazy,
            &fresh,
            &[MemoryCommand::Forget { id }],
        )
        .unwrap();
        assert_eq!(applied.snapshot.records()[0].state, MemoryState::Forgotten);
    }

    #[cfg(feature = "sqlite-system")]
    #[test]
    fn reset_epoch_prevents_pre_restart_memory_snapshot_from_writing() {
        use crate::{
            identity::EventId,
            timeline::{
                TimelineConfig,
                domain::{BranchId, SessionId, TimelineError},
            },
        };

        let path = std::env::temp_dir().join(format!(
            "neuromita-memory-epoch-{}.db",
            uuid::Uuid::new_v4(),
        ));
        let session = SessionId::new();
        let root = BranchId::new();
        let stale = {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let initial = super::read_memory_snapshot(&timeline, CharacterId::Crazy).unwrap();
            let result = super::edit_memory_snapshot(
                &mut timeline,
                CharacterId::Crazy,
                &initial,
                &[MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Durable".into(),
                }],
            )
            .unwrap();
            timeline
                .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
                .unwrap();
            result.snapshot
        };
        {
            let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
                database: path.clone(),
                session,
                root,
            })
            .unwrap();
            let id = stale.records()[0].id;
            assert_eq!(
                super::edit_memory_snapshot(
                    &mut timeline,
                    CharacterId::Crazy,
                    &stale,
                    &[MemoryCommand::Forget { id }],
                )
                .err(),
                Some(TimelineError::ConcurrentWrite),
            );
            let fresh = super::read_memory_snapshot(&timeline, CharacterId::Crazy).unwrap();
            assert_eq!(fresh.records()[0].content, "Durable");
            assert_ne!(fresh.context_epoch, stale.context_epoch);
            assert!(
                super::edit_memory_snapshot(
                    &mut timeline,
                    CharacterId::Crazy,
                    &fresh,
                    &[MemoryCommand::Forget { id }],
                )
                .is_ok()
            );
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[tokio::test]
    async fn replayed_values_cannot_resurrect_a_stale_memory_editor() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let initial = mita
            .edit_memories(
                mita.memories().await.unwrap(),
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Original".into(),
                }],
            )
            .await
            .unwrap();
        let old = initial.snapshot.clone();
        let id = old.records()[0].id;
        let changed = mita
            .edit_memories(
                initial.snapshot,
                vec![MemoryCommand::Update {
                    id,
                    priority: None,
                    content: "Changed".into(),
                }],
            )
            .await
            .unwrap();
        let restored = mita
            .edit_memories(
                changed.snapshot,
                vec![MemoryCommand::Update {
                    id,
                    priority: None,
                    content: "Original".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(old.records(), restored.snapshot.records());
        assert!(matches!(
            mita.edit_memories(old, vec![MemoryCommand::Forget { id }])
                .await,
            Err(CharacterError::StaleState)
        ));
        assert_eq!(
            mita.memories().await.unwrap().records()[0].state,
            MemoryState::Active
        );
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn unrelated_timeline_and_other_character_writes_do_not_invalidate_memory_editor() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let mita = registry.get(CharacterId::Crazy).unwrap();
        let kind = registry.get(CharacterId::Kind).unwrap();
        let old_mita = mita.memories().await.unwrap();
        kind.edit_memories(
            kind.memories().await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "Other character's fact".into(),
            }],
        )
        .await
        .unwrap();
        let result = mita
            .edit_memories(
                old_mita,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Own character's fact".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(result.snapshot.records().len(), 1);
        assert_eq!(kind.memories().await.unwrap().records().len(), 1);
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }
}
