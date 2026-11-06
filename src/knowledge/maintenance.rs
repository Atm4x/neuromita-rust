//! Explicit, confirmed graph maintenance over an immutable character Timeline snapshot.
//! Inference and preview do not write; the character owner commits at most one batch.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterMemorySnapshot, MemoryEditResult},
    memory::{MAX_MEMORY_EDIT_CHANGES, MemoryCommand, MemoryId, MemoryRelation},
};

use super::graph::{GraphError, GraphExtractionSource, GraphExtractor, propose_extraction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphMaintenanceMode {
    Clear,
    Reindex,
}

#[derive(Debug, Clone)]
pub struct GraphMaintenanceEntry {
    pub source: GraphExtractionSource,
    pub existing_relations: Vec<MemoryRelation>,
}

#[derive(Debug, Clone)]
pub struct GraphMaintenancePreview {
    expected: CharacterMemorySnapshot,
    entries: Vec<GraphMaintenanceEntry>,
    mode: GraphMaintenanceMode,
}

impl GraphMaintenancePreview {
    pub fn entries(&self) -> &[GraphMaintenanceEntry] {
        &self.entries
    }

    pub fn mode(&self) -> GraphMaintenanceMode {
        self.mode
    }
}

#[derive(Debug, Error)]
pub enum GraphMaintenanceError {
    #[error("graph maintenance requires 1..256 unique source IDs")]
    InvalidSelection,
    #[error("graph maintenance source is missing, inactive or expired")]
    InvalidSource,
    #[error("graph maintenance requires explicit confirmation")]
    ConfirmationRequired,
    #[error("graph maintenance extractor is unavailable")]
    BackendUnavailable,
    #[error("graph maintenance cancelled before commit")]
    Cancelled,
    #[error("graph extraction failed: {0:?}")]
    Extraction(GraphError),
    #[error("graph maintenance write failed: {0}")]
    Write(#[from] CharacterError),
}

/// Build a read-only preview for exact source IDs. The as-of timestamp is
/// injected by the caller; the knowledge domain does not consult wall-clock time.
pub fn preview_graph_maintenance(
    expected: CharacterMemorySnapshot,
    ids: &[MemoryId],
    as_of_unix_ms: u64,
    mode: GraphMaintenanceMode,
) -> Result<GraphMaintenancePreview, GraphMaintenanceError> {
    if ids.is_empty() || ids.len() > MAX_MEMORY_EDIT_CHANGES {
        return Err(GraphMaintenanceError::InvalidSelection);
    }
    let mut seen = BTreeSet::new();
    if ids.iter().any(|id| !seen.insert(*id)) {
        return Err(GraphMaintenanceError::InvalidSelection);
    }
    let mut existing = BTreeMap::<MemoryId, Vec<MemoryRelation>>::new();
    for item in expected.extracted_relations() {
        existing
            .entry(item.source)
            .or_default()
            .push(item.relation.clone());
    }
    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let record = expected
            .records()
            .iter()
            .find(|record| record.id == *id && record.available_at(as_of_unix_ms))
            .ok_or(GraphMaintenanceError::InvalidSource)?;
        let revision = expected
            .source_revision(*id)
            .ok_or(GraphMaintenanceError::InvalidSource)?;
        let source = GraphExtractionSource::new(record.clone(), revision)
            .map_err(|_| GraphMaintenanceError::InvalidSource)?;
        entries.push(GraphMaintenanceEntry {
            source,
            existing_relations: existing.remove(id).unwrap_or_default(),
        });
    }
    Ok(GraphMaintenancePreview {
        expected,
        entries,
        mode,
    })
}

/// Confirmed and atomic graph maintenance. Any provider error or cancellation
/// before the write boundary abandons every proposed change. After dispatch to
/// the character owner the guarded Timeline append owns completion semantics.
pub async fn apply_graph_maintenance(
    owner: &CharacterHandle,
    plan: GraphMaintenancePreview,
    extractor: Option<&dyn GraphExtractor>,
    confirmed: bool,
    cancel: &CancellationToken,
) -> Result<MemoryEditResult, GraphMaintenanceError> {
    if !confirmed {
        return Err(GraphMaintenanceError::ConfirmationRequired);
    }
    if cancel.is_cancelled() {
        return Err(GraphMaintenanceError::Cancelled);
    }
    if plan.mode == GraphMaintenanceMode::Reindex && extractor.is_none() {
        return Err(GraphMaintenanceError::BackendUnavailable);
    }
    let mut commands = Vec::with_capacity(plan.entries.len());
    for entry in &plan.entries {
        let command = match plan.mode {
            GraphMaintenanceMode::Clear => MemoryCommand::ReplaceExtractedRelations {
                source: entry.source.record.id,
                expected_revision: entry.source.revision,
                relations: Vec::new(),
            },
            GraphMaintenanceMode::Reindex => {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(GraphMaintenanceError::Cancelled),
                    result = propose_extraction(extractor.expect("checked above"), &entry.source) => {
                        result.map_err(GraphMaintenanceError::Extraction)?
                    }
                }
            }
        };
        commands.push(command);
    }
    if cancel.is_cancelled() {
        return Err(GraphMaintenanceError::Cancelled);
    }
    owner
        .edit_memories(plan.expected, commands)
        .await
        .map_err(GraphMaintenanceError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character::{CharacterId, CharacterRegistry},
        knowledge::graph::GraphExtractionFuture,
        memory::{MemoryPriority, MemoryRecord},
        timeline::SessionTimeline,
    };
    use std::sync::{Arc, Mutex};

    struct FixtureExtractor {
        fail_on: Option<&'static str>,
    }

    impl GraphExtractor for FixtureExtractor {
        fn extract<'a>(&'a self, source: &'a MemoryRecord) -> GraphExtractionFuture<'a> {
            Box::pin(async move {
                if self
                    .fail_on
                    .is_some_and(|needle| source.content.contains(needle))
                {
                    return Err(GraphError::Provider("intentional test error".into()));
                }
                let object = if source.content.contains("coffee") {
                    "coffee"
                } else {
                    "tea"
                };
                Ok(vec![MemoryRelation {
                    subject: "Mita".into(),
                    predicate: "likes".into(),
                    object: object.into(),
                }])
            })
        }
    }

    #[tokio::test]
    async fn graph_maintenance_is_confirmed_previewable_idempotent_and_atomic() {
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
                vec![
                    MemoryCommand::Add {
                        priority: MemoryPriority::Normal,
                        content: "Mita likes tea".into(),
                    },
                    MemoryCommand::Add {
                        priority: MemoryPriority::High,
                        content: "Mita likes coffee".into(),
                    },
                ],
            )
            .await
            .unwrap();
        let ids = initial
            .snapshot
            .records()
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        assert!(matches!(
            preview_graph_maintenance(
                initial.snapshot.clone(),
                &[ids[0], ids[0]],
                0,
                GraphMaintenanceMode::Clear
            ),
            Err(GraphMaintenanceError::InvalidSelection)
        ));
        let preview = preview_graph_maintenance(
            initial.snapshot.clone(),
            &ids,
            0,
            GraphMaintenanceMode::Reindex,
        )
        .unwrap();
        assert_eq!(preview.entries().len(), 2);
        assert!(
            preview
                .entries()
                .iter()
                .all(|item| item.existing_relations.is_empty())
        );
        let extractor = FixtureExtractor { fail_on: None };
        assert!(matches!(
            apply_graph_maintenance(&mita, preview.clone(), Some(&extractor), false, &stop).await,
            Err(GraphMaintenanceError::ConfirmationRequired)
        ));
        assert!(matches!(
            apply_graph_maintenance(&mita, preview.clone(), None, true, &stop).await,
            Err(GraphMaintenanceError::BackendUnavailable)
        ));
        assert!(
            mita.memories()
                .await
                .unwrap()
                .extracted_relations()
                .is_empty()
        );
        let indexed =
            apply_graph_maintenance(&mita, preview.clone(), Some(&extractor), true, &stop)
                .await
                .unwrap();
        assert_eq!(indexed.applied.len(), 2);
        assert_eq!(indexed.snapshot.extracted_relations().len(), 2);
        assert!(matches!(
            apply_graph_maintenance(&mita, preview, Some(&extractor), true, &stop).await,
            Err(GraphMaintenanceError::Write(_))
        ));
        let clear_preview =
            preview_graph_maintenance(indexed.snapshot, &ids, 0, GraphMaintenanceMode::Clear)
                .unwrap();
        assert!(
            clear_preview
                .entries()
                .iter()
                .all(|item| item.existing_relations.len() == 1)
        );
        let cleared = apply_graph_maintenance(&mita, clear_preview, None, true, &stop)
            .await
            .unwrap();
        assert_eq!(cleared.applied.len(), 2);
        assert!(cleared.snapshot.extracted_relations().is_empty());
        let clear_again =
            preview_graph_maintenance(cleared.snapshot, &ids, 0, GraphMaintenanceMode::Clear)
                .unwrap();
        let repeated = apply_graph_maintenance(&mita, clear_again, None, true, &stop)
            .await
            .unwrap();
        assert!(repeated.applied.is_empty());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[tokio::test]
    async fn graph_reindex_and_clear_survive_sqlite_reopen() {
        use crate::timeline::{
            TimelineConfig,
            domain::{BranchId, SessionId},
        };

        let path = std::env::temp_dir().join(format!(
            "neuromita-graph-maintenance-{}.sqlite",
            uuid::Uuid::new_v4(),
        ));
        let session = SessionId::new();
        let root = BranchId::new();
        let config = || TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        };
        let id;
        {
            let stop = CancellationToken::new();
            let timeline = SessionTimeline::from_config(config()).unwrap();
            let (registry, tasks) =
                CharacterRegistry::spawn(Arc::new(Mutex::new(timeline)), stop.clone(), 16);
            let mita = registry.get(CharacterId::Crazy).unwrap();
            let added = mita
                .edit_memories(
                    mita.memories().await.unwrap(),
                    vec![MemoryCommand::Add {
                        priority: MemoryPriority::Normal,
                        content: "Mita likes tea".into(),
                    }],
                )
                .await
                .unwrap();
            id = added.snapshot.records()[0].id;
            let preview =
                preview_graph_maintenance(added.snapshot, &[id], 0, GraphMaintenanceMode::Reindex)
                    .unwrap();
            let indexed = apply_graph_maintenance(
                &mita,
                preview,
                Some(&FixtureExtractor { fail_on: None }),
                true,
                &stop,
            )
            .await
            .unwrap();
            assert_eq!(indexed.snapshot.extracted_relations().len(), 1);
            stop.cancel();
            for task in tasks {
                task.await.unwrap();
            }
        }
        {
            let stop = CancellationToken::new();
            let timeline = SessionTimeline::from_config(config()).unwrap();
            let (registry, tasks) =
                CharacterRegistry::spawn(Arc::new(Mutex::new(timeline)), stop.clone(), 16);
            let mita = registry.get(CharacterId::Crazy).unwrap();
            let snapshot = mita.memories().await.unwrap();
            assert_eq!(snapshot.extracted_relations().len(), 1);
            let plan =
                preview_graph_maintenance(snapshot, &[id], 0, GraphMaintenanceMode::Clear).unwrap();
            apply_graph_maintenance(&mita, plan, None, true, &stop)
                .await
                .unwrap();
            stop.cancel();
            for task in tasks {
                task.await.unwrap();
            }
        }
        {
            let stop = CancellationToken::new();
            let timeline = SessionTimeline::from_config(config()).unwrap();
            let (registry, tasks) =
                CharacterRegistry::spawn(Arc::new(Mutex::new(timeline)), stop.clone(), 16);
            let mita = registry.get(CharacterId::Crazy).unwrap();
            let snapshot = mita.memories().await.unwrap();
            assert_eq!(snapshot.records()[0].id, id);
            assert!(snapshot.extracted_relations().is_empty());
            stop.cancel();
            for task in tasks {
                task.await.unwrap();
            }
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[tokio::test]
    async fn partial_failure_cancellation_and_stale_preview_leave_no_committed_relations() {
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
                vec![
                    MemoryCommand::Add {
                        priority: MemoryPriority::Normal,
                        content: "Mita likes tea".into(),
                    },
                    MemoryCommand::Add {
                        priority: MemoryPriority::Normal,
                        content: "Mita likes coffee".into(),
                    },
                ],
            )
            .await
            .unwrap();
        let ids = initial
            .snapshot
            .records()
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        let preview = preview_graph_maintenance(
            initial.snapshot.clone(),
            &ids,
            0,
            GraphMaintenanceMode::Reindex,
        )
        .unwrap();
        assert!(matches!(
            apply_graph_maintenance(
                &mita,
                preview.clone(),
                Some(&FixtureExtractor {
                    fail_on: Some("coffee")
                }),
                true,
                &stop
            )
            .await,
            Err(GraphMaintenanceError::Extraction(GraphError::Provider(_)))
        ));
        assert!(
            mita.memories()
                .await
                .unwrap()
                .extracted_relations()
                .is_empty()
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            apply_graph_maintenance(
                &mita,
                preview.clone(),
                Some(&FixtureExtractor { fail_on: None }),
                true,
                &cancelled
            )
            .await,
            Err(GraphMaintenanceError::Cancelled)
        ));
        assert!(
            mita.memories()
                .await
                .unwrap()
                .extracted_relations()
                .is_empty()
        );
        let _changed = mita
            .edit_memories(
                initial.snapshot,
                vec![MemoryCommand::Update {
                    id: ids[0],
                    priority: None,
                    content: "Mita likes fruit tea".into(),
                }],
            )
            .await
            .unwrap();
        assert!(matches!(
            apply_graph_maintenance(
                &mita,
                preview,
                Some(&FixtureExtractor { fail_on: None }),
                true,
                &stop
            )
            .await,
            Err(GraphMaintenanceError::Write(_))
        ));
        assert!(
            mita.memories()
                .await
                .unwrap()
                .extracted_relations()
                .is_empty()
        );
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }
}
