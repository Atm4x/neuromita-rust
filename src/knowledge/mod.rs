//! Deterministic knowledge selection over caller-owned, snapshot-scoped facts.
//! The embedding port is an abstraction; provider implementations live outside this module.

pub mod embedding;
pub mod forgotten_recall;
pub mod graph;
pub mod graph_retrieval;
pub mod history_generation;
pub mod history_retrieval;
pub mod history_window;
pub mod maintenance;
pub mod memory_context;
pub mod memory_transfer;
pub mod reranking;
pub mod retrieval_fusion;
pub mod semantic_memory;

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::{
    character::CharacterMemorySnapshot,
    identity::CharacterId,
    memory::{MemoryKind, MemoryRelation, MemoryState},
    timeline::domain::MemoryRecord,
};
use embedding::EmbeddingError;
use forgotten_recall::{ForgottenRecall, MAX_RECALL_RESULTS};
use memory_context::{
    lexical_ranked_scoped, render_memory_context, render_memory_islands,
    render_ranked_memory_context_scoped, render_relation_context,
};
use retrieval_fusion::{FusionConfig, FusionMode, fuse};
use semantic_memory::{
    EmbeddingCacheMaintenanceError, EmbeddingCachePreview, SemanticMemoryRetriever,
    SemanticRetrievalFilter,
};

/// Explicit selection policy. Knowledge does not own canonical Timeline records or model runtimes.
pub struct KnowledgeRetrieval {
    mode: KnowledgeMode,
}

enum KnowledgeMode {
    Lexical,
    Semantic {
        retriever: Arc<SemanticMemoryRetriever>,
        fusion: FusionMode,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum HistoricalMemorySelectionError {
    #[error("forgotten recall belongs to another character or Timeline prefix")]
    StaleRecall,
    #[error("forgotten recall contains invalid, duplicate or changed memories")]
    InvalidRecall,
    #[error("historical semantic retrieval failed: {0}")]
    Embedding(#[from] EmbeddingError),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KnowledgeCacheClearError {
    #[error("semantic embedding cache is not configured")]
    BackendUnavailable,
    #[error(transparent)]
    Maintenance(#[from] EmbeddingCacheMaintenanceError),
}

/// Asynchronous historical retrieval cannot expose prompt text until the owner
/// re-reads its current authoritative Timeline projection and verifies that the
/// original read is still valid. Constructed only by KnowledgeRetrieval.
#[must_use = "historical recall must be finalized against a fresh Timeline snapshot"]
pub struct PendingHistoricalRecall {
    captured: CharacterMemorySnapshot,
    rendered: Option<String>,
}

impl PendingHistoricalRecall {
    /// The caller must obtain `current` again from its live CharacterHandle after
    /// retrieval, immediately before using the text. A stale branch, Timeline
    /// position, context reset or memory mutation rejects the whole result.
    pub fn finalize(
        self,
        current: &CharacterMemorySnapshot,
    ) -> Result<Option<String>, HistoricalMemorySelectionError> {
        if self.captured.anchor != current.anchor || !self.captured.matches_version(current) {
            return Err(HistoricalMemorySelectionError::StaleRecall);
        }
        Ok(self.rendered)
    }
}

impl KnowledgeRetrieval {
    /// Explicitly rank *only* the forgotten facts selected by a prior recall
    /// action, tied to one character and one immutable Timeline prefix.
    /// The regular generation selection never calls this method. A request
    /// owner must separately authorize the original historical recall, then
    /// finalize the opaque result against a freshly read Timeline snapshot.
    pub async fn select_recalled_forgotten(
        &self,
        snapshot: &CharacterMemorySnapshot,
        recalled: &ForgottenRecall,
        query: &str,
    ) -> Result<PendingHistoricalRecall, HistoricalMemorySelectionError> {
        if recalled.character != snapshot.character() || recalled.anchor != snapshot.anchor() {
            return Err(HistoricalMemorySelectionError::StaleRecall);
        }
        if recalled.facts.len() > MAX_RECALL_RESULTS {
            return Err(HistoricalMemorySelectionError::InvalidRecall);
        }
        let mut ids = BTreeSet::new();
        let mut selected = Vec::with_capacity(recalled.facts.len());
        for item in &recalled.facts {
            let record = &item.record;
            if !ids.insert(record.id)
                || record.kind != MemoryKind::Fact
                || record.state != MemoryState::Forgotten
                || !snapshot.records().iter().any(|current| current == record)
            {
                return Err(HistoricalMemorySelectionError::InvalidRecall);
            }
            selected.push(record.clone());
        }
        let rendered = match &self.mode {
            KnowledgeMode::Lexical => {
                render_ranked_memory_context_scoped(
                    lexical_ranked_scoped(&selected, query, true).into_iter(),
                    true,
                )
                .0
            }
            KnowledgeMode::Semantic { retriever, fusion } => {
                retriever
                    .retrieve_recalled_forgotten(snapshot.character(), selected, query, *fusion)
                    .await?
                    .rendered
            }
        };
        Ok(PendingHistoricalRecall {
            captured: snapshot.clone(),
            rendered,
        })
    }

    /// Explicit History vector/fusion read. Lexical configuration is not a
    /// substitute for an unavailable embedding backend.
    pub async fn search_history_semantic(
        &self,
        character: CharacterId,
        view: &crate::timeline::view::MitaView,
        query: &str,
        config: history_retrieval::HistorySearchConfig,
        mode: FusionMode,
        minimum_cosine: Option<f64>,
    ) -> Result<history_retrieval::HistorySearchResult, history_retrieval::HistorySearchError> {
        let KnowledgeMode::Semantic { retriever, .. } = &self.mode else {
            return Err(history_retrieval::HistorySearchError::BackendUnavailable);
        };
        history_retrieval::search_history_semantic(
            retriever,
            character,
            view,
            query,
            config,
            mode,
            minimum_cosine,
        )
        .await
    }

    /// Same live semantic owner, but only the older messages excluded by the
    /// admission-pinned conversation window may become retrieval evidence.
    pub async fn search_archived_history_semantic(
        &self,
        character: CharacterId,
        view: &crate::timeline::view::MitaView,
        window: &history_window::HistoryWindow,
        query: &str,
        config: history_retrieval::HistorySearchConfig,
        mode: FusionMode,
        minimum_cosine: Option<f64>,
    ) -> Result<history_retrieval::HistorySearchResult, history_retrieval::HistorySearchError> {
        let KnowledgeMode::Semantic { retriever, .. } = &self.mode else {
            return Err(history_retrieval::HistorySearchError::BackendUnavailable);
        };
        history_retrieval::search_archived_history_semantic(
            retriever,
            character,
            view,
            window,
            query,
            config,
            mode,
            minimum_cosine,
        )
        .await
    }

    /// Preview the single derived embedding cache belonging to the running
    /// Knowledge owner. No canonical memories or Timeline events are exposed.
    pub fn preview_embedding_cache_clear(&self) -> Option<EmbeddingCachePreview> {
        match &self.mode {
            KnowledgeMode::Lexical => None,
            KnowledgeMode::Semantic { retriever, .. } => Some(retriever.preview_cache_clear()),
        }
    }

    /// Perform an explicitly confirmed clear against an exact prior preview.
    /// A stale preview cannot delete newer cache entries.
    pub fn clear_embedding_cache_confirmed(
        &self,
        preview: &EmbeddingCachePreview,
        confirmed: bool,
    ) -> Result<usize, KnowledgeCacheClearError> {
        let KnowledgeMode::Semantic { retriever, .. } = &self.mode else {
            return Err(KnowledgeCacheClearError::BackendUnavailable);
        };
        Ok(retriever.clear_cache_confirmed(preview, confirmed)?)
    }

    pub fn lexical() -> Self {
        Self {
            mode: KnowledgeMode::Lexical,
        }
    }

    pub fn semantic(retriever: Arc<SemanticMemoryRetriever>) -> Self {
        Self {
            mode: KnowledgeMode::Semantic {
                retriever,
                fusion: FusionMode::VectorOnly,
            },
        }
    }

    pub fn semantic_fusion(
        retriever: Arc<SemanticMemoryRetriever>,
        fusion: FusionMode,
    ) -> Result<Self, EmbeddingError> {
        fuse(
            CharacterId::Crazy,
            &[],
            FusionConfig {
                mode: fusion,
                limit: 1024,
                minimum_graph: 0,
            },
        )
        .map_err(|error| EmbeddingError::Configuration(error.to_string()))?;
        Ok(Self {
            mode: KnowledgeMode::Semantic { retriever, fusion },
        })
    }

    /// The existing fact-only selection contract remains available to callers.
    pub async fn select(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
    ) -> Result<Option<String>, EmbeddingError> {
        self.select_with_islands(character, memories, &[], query)
            .await
    }

    pub async fn select_with_relations(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        islands: &[MemoryRecord],
        relations: &[MemoryRelation],
        query: &str,
    ) -> Result<Option<String>, EmbeddingError> {
        let selected = self
            .select_with_islands(character, memories, islands, query)
            .await?;
        let links = match &self.mode {
            KnowledgeMode::Semantic { .. } => render_relation_context(relations, query),
            KnowledgeMode::Lexical => None,
        };
        Ok(match (selected, links) {
            (Some(text), Some(links)) => Some(format!("{text}\n{links}")),
            (None, Some(links)) => Some(links),
            (text, None) => text,
        })
    }

    /// Islands are rendered separately from ranked facts and never embedded.
    /// Every input is a caller-owned snapshot, not a second memory store.
    pub async fn select_with_islands(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        islands: &[MemoryRecord],
        query: &str,
    ) -> Result<Option<String>, EmbeddingError> {
        let selected = match &self.mode {
            KnowledgeMode::Lexical => render_memory_context(memories, query),
            KnowledgeMode::Semantic { retriever, fusion } => {
                let result = retriever
                    .retrieve_with_fusion(
                        character,
                        memories,
                        query,
                        SemanticRetrievalFilter::all(),
                        *fusion,
                    )
                    .await?;
                tracing::debug!(
                    character = %character,
                    selected = result.selected_ids.len(),
                    embedding_requests = result.cost.embedding_requests,
                    embedded_texts = result.cost.embedded_texts,
                    embedded_characters = result.cost.embedded_characters,
                    cache_hits = result.cost.cache_hits,
                    "knowledge retrieval finished"
                );
                result.rendered
            }
        };
        let islands = render_memory_islands(islands);
        Ok(match (islands, selected) {
            (Some(islands), Some(facts)) => Some(format!("{islands}\n{facts}")),
            (Some(islands), None) => Some(islands),
            (None, facts) => facts,
        })
    }
}

impl Default for KnowledgeRetrieval {
    fn default() -> Self {
        Self::lexical()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::historical_recall::{ExplicitRecallError, recall_from_character},
        character::CharacterMemorySnapshot,
        knowledge::forgotten_recall::{RecallError, RecallSelector, recall_forgotten},
        memory::{CharacterMemories, MemoryChange, MemoryState},
        timeline::{
            ContextAnchor,
            domain::{BranchId, MemoryId, MemoryPriority},
            selection::TimelineSelection,
        },
    };
    use embedding::{
        Embedding, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    struct UnavailableFixture {
        space: EmbeddingSpace,
        calls: AtomicUsize,
    }

    impl EmbeddingProvider for UnavailableFixture {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(&self, _: EmbeddingPurpose, _: String) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Err(EmbeddingError::Transport("fixture not available".into()))
            })
        }
    }

    fn fact() -> MemoryRecord {
        MemoryRecord {
            entities: Vec::new(),
            id: MemoryId(Uuid::from_u128(14)),
            priority: MemoryPriority::Normal,
            content: "ромашковый чай".into(),
            kind: crate::memory::MemoryKind::Fact,
            state: crate::memory::MemoryState::Active,
            protected: false,
            expires_at_unix_ms: None,
        }
    }

    fn recall_fixture() -> (CharacterMemorySnapshot, ForgottenRecall) {
        let anchor = ContextAnchor {
            branch_id: Uuid::from_u128(0xaaaa),
            through: 17,
        };
        let mut active = fact();
        active.id = MemoryId(Uuid::from_u128(10));
        active.content = "active ромашковый чай".into();
        let mut selected = fact();
        selected.id = MemoryId(Uuid::from_u128(11));
        selected.content = "forgotten ромашковый чай".into();
        selected.state = MemoryState::Forgotten;
        let mut unselected = fact();
        unselected.id = MemoryId(Uuid::from_u128(12));
        unselected.content = "unselected ромашковый чай".into();
        unselected.state = MemoryState::Forgotten;
        let mut book = CharacterMemories::default();
        for record in [&active, &selected, &unselected] {
            book.apply_committed(&MemoryChange::Upsert(record.clone()))
                .unwrap();
        }
        let recall = recall_forgotten(
            CharacterId::Crazy,
            anchor,
            &book,
            RecallSelector::Ids(vec![selected.id]),
        )
        .unwrap();
        let snapshot = CharacterMemorySnapshot {
            character: CharacterId::Crazy,
            anchor,
            records: book.all(),
            relations: Vec::new(),
            extracted_relations: Vec::new(),
            memory_revision: None,
            context_epoch: 0,
            source_revisions: book.source_revisions(),
            selection: TimelineSelection {
                branch_id: BranchId(anchor.branch_id),
                generation: 1,
            },
        };
        (snapshot, recall)
    }

    #[tokio::test]
    async fn explicit_recall_never_expands_beyond_selected_ids() {
        let (snapshot, recall) = recall_fixture();
        let service = KnowledgeRetrieval::lexical();
        let historical = service
            .select_recalled_forgotten(&snapshot, &recall, "ромашковый чай")
            .await
            .unwrap()
            .finalize(&snapshot)
            .unwrap()
            .unwrap();
        assert!(historical.contains("forgotten ромашковый чай"));
        assert!(historical.contains("[FORGOTTEN]"));
        assert!(!historical.contains("active ромашковый чай"));
        assert!(!historical.contains("unselected ромашковый чай"));
        let normal = service
            .select(CharacterId::Crazy, snapshot.records(), "ромашковый чай")
            .await
            .unwrap()
            .unwrap();
        assert!(normal.contains("active ромашковый чай"));
        assert!(!normal.contains("forgotten ромашковый чай"));
    }

    #[tokio::test]
    async fn recall_selection_rejects_foreign_stale_and_tampered_evidence() {
        let (snapshot, recall) = recall_fixture();
        let service = KnowledgeRetrieval::lexical();
        let mut foreign = recall.clone();
        foreign.character = CharacterId::Kind;
        assert!(matches!(
            service
                .select_recalled_forgotten(&snapshot, &foreign, "чай")
                .await,
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));
        let mut stale = recall.clone();
        stale.anchor.through += 1;
        assert!(matches!(
            service
                .select_recalled_forgotten(&snapshot, &stale, "чай")
                .await,
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));
        let mut changed = recall.clone();
        changed.facts[0].record.content = "modified payload".into();
        assert!(matches!(
            service
                .select_recalled_forgotten(&snapshot, &changed, "чай")
                .await,
            Err(HistoricalMemorySelectionError::InvalidRecall)
        ));
        let mut duplicate = recall.clone();
        duplicate.facts.push(recall.facts[0].clone());
        assert!(matches!(
            service
                .select_recalled_forgotten(&snapshot, &duplicate, "чай")
                .await,
            Err(HistoricalMemorySelectionError::InvalidRecall)
        ));
        let mut restored = snapshot.clone();
        let target = restored
            .records
            .iter_mut()
            .find(|r| r.id == recall.facts[0].record.id)
            .unwrap();
        target.state = MemoryState::Active;
        assert!(matches!(
            service
                .select_recalled_forgotten(&restored, &recall, "чай")
                .await,
            Err(HistoricalMemorySelectionError::InvalidRecall)
        ));
    }

    struct SuccessfulFixture {
        space: EmbeddingSpace,
        query_calls: AtomicUsize,
        document_calls: AtomicUsize,
    }

    impl EmbeddingProvider for SuccessfulFixture {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(&self, purpose: EmbeddingPurpose, _: String) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async move {
                if purpose == EmbeddingPurpose::Query {
                    self.query_calls.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.document_calls.fetch_add(1, Ordering::Relaxed);
                }
                Embedding::new(vec![1.0, 0.0], &self.space)
            })
        }
    }

    #[tokio::test]
    async fn semantic_recall_embeds_only_explicit_forgotten_selection() {
        let (snapshot, recall) = recall_fixture();
        let provider = Arc::new(SuccessfulFixture {
            space: EmbeddingSpace::new("fixture", "recall", "v1", 2).unwrap(),
            query_calls: AtomicUsize::new(0),
            document_calls: AtomicUsize::new(0),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 10).unwrap());
        let selected = KnowledgeRetrieval::semantic(retriever)
            .select_recalled_forgotten(&snapshot, &recall, "ромашковый чай")
            .await
            .unwrap()
            .finalize(&snapshot)
            .unwrap()
            .unwrap();
        assert!(selected.contains("forgotten ромашковый чай"));
        assert!(!selected.contains("unselected ромашковый чай"));
        assert!(!selected.contains("active ромашковый чай"));
        assert_eq!(provider.query_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.document_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn invalid_historical_selection_never_calls_embedding_backend() {
        let (snapshot, mut recall) = recall_fixture();
        recall.facts[0].record.content = "forged record".into();
        let fixture = Arc::new(UnavailableFixture {
            space: EmbeddingSpace::new("fixture", "disabled", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 10).unwrap());
        let selection = KnowledgeRetrieval::semantic(retriever)
            .select_recalled_forgotten(&snapshot, &recall, "чай")
            .await;
        assert!(matches!(
            selection,
            Err(HistoricalMemorySelectionError::InvalidRecall)
        ));
        assert_eq!(fixture.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn completed_recall_rejects_changed_timeline_or_character_before_prompt_use() {
        let (snapshot, recall) = recall_fixture();
        let retrieval = KnowledgeRetrieval::lexical();
        let pending = retrieval
            .select_recalled_forgotten(&snapshot, &recall, "чай")
            .await
            .unwrap();
        let mut advanced = snapshot.clone();
        advanced.anchor.through += 1;
        assert!(matches!(
            pending.finalize(&advanced),
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));

        let mut changed = snapshot.clone();
        changed.context_epoch += 1;
        let pending = retrieval
            .select_recalled_forgotten(&snapshot, &recall, "чай")
            .await
            .unwrap();
        assert!(matches!(
            pending.finalize(&changed),
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));

        let mut other = snapshot.clone();
        other.character = CharacterId::Kind;
        let pending = retrieval
            .select_recalled_forgotten(&snapshot, &recall, "чай")
            .await
            .unwrap();
        assert!(matches!(
            pending.finalize(&other),
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));

        let mut edited = snapshot.clone();
        edited.records[0].content = "changed after retrieval".into();
        let pending = retrieval
            .select_recalled_forgotten(&snapshot, &recall, "чай")
            .await
            .unwrap();
        assert!(matches!(
            pending.finalize(&edited),
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));

        let pending = retrieval
            .select_recalled_forgotten(&snapshot, &recall, "чай")
            .await
            .unwrap();
        assert!(
            pending
                .finalize(&snapshot)
                .unwrap()
                .unwrap()
                .contains("[FORGOTTEN]")
        );
    }

    struct PausedQueryFixture {
        space: EmbeddingSpace,
        started: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Notify>,
    }

    impl EmbeddingProvider for PausedQueryFixture {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(&self, purpose: EmbeddingPurpose, _: String) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async move {
                if purpose == EmbeddingPurpose::Query {
                    self.started.notify_one();
                    self.resume.notified().await;
                }
                Embedding::new(vec![1.0, 0.0], &self.space)
            })
        }
    }

    #[tokio::test]
    async fn in_flight_semantic_recall_cannot_reveal_text_after_timeline_advances() {
        let (snapshot, recall) = recall_fixture();
        let started = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(PausedQueryFixture {
            space: EmbeddingSpace::new("fixture", "delayed-recall", "v1", 2).unwrap(),
            started: started.clone(),
            resume: resume.clone(),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(provider, 10).unwrap());
        let original = snapshot.clone();
        let task = tokio::spawn(async move {
            KnowledgeRetrieval::semantic(retriever)
                .select_recalled_forgotten(&original, &recall, "чай")
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        let mut advanced = snapshot.clone();
        advanced.anchor.through += 1;
        resume.notify_one();
        let pending = task.await.unwrap().unwrap();
        assert!(matches!(
            pending.finalize(&advanced),
            Err(HistoricalMemorySelectionError::StaleRecall)
        ));
    }

    #[tokio::test]
    async fn explicit_character_action_reads_live_owner_without_exposing_unselected_facts() {
        use crate::{
            character::CharacterRegistry, memory::MemoryCommand, timeline::SessionTimeline,
        };
        use std::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let owner = registry.get(CharacterId::Crazy).unwrap();
        let other = registry.get(CharacterId::Kind).unwrap();
        let first = owner.memories().await.unwrap();
        let added = owner
            .edit_memories(
                first,
                vec![
                    MemoryCommand::Add {
                        priority: MemoryPriority::Normal,
                        content: "secret forgotten orange tea".into(),
                    },
                    MemoryCommand::Add {
                        priority: MemoryPriority::Normal,
                        content: "other forgotten purple tea".into(),
                    },
                ],
            )
            .await
            .unwrap();
        let ids = added
            .snapshot
            .records()
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        let selected_id = added
            .snapshot
            .records()
            .iter()
            .find(|record| record.content == "secret forgotten orange tea")
            .unwrap()
            .id;
        let first_forgotten = owner
            .edit_memories(
                added.snapshot,
                vec![
                    MemoryCommand::Forget { id: ids[0] },
                    MemoryCommand::Forget { id: ids[1] },
                ],
            )
            .await
            .unwrap();
        assert!(
            first_forgotten
                .snapshot
                .records()
                .iter()
                .all(|r| r.state == MemoryState::Forgotten)
        );
        let query = KnowledgeRetrieval::lexical();
        let selected = recall_from_character(
            &query,
            &owner,
            RecallSelector::Ids(vec![selected_id]),
            "tea",
        )
        .await
        .unwrap()
        .unwrap();
        assert!(selected.contains("secret forgotten orange tea"));
        assert!(!selected.contains("other forgotten purple tea"));
        assert!(selected.contains("[FORGOTTEN]"));
        assert!(matches!(
            recall_from_character(
                &query,
                &other,
                RecallSelector::Ids(vec![selected_id]),
                "tea"
            )
            .await,
            Err(ExplicitRecallError::Selector(RecallError::IneligibleId))
        ));
        assert!(
            owner
                .memories()
                .await
                .unwrap()
                .records()
                .iter()
                .all(|r| r.state == MemoryState::Forgotten)
        );
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn explicit_character_action_rechecks_live_timeline_after_async_embedding() {
        use crate::{
            character::CharacterRegistry, memory::MemoryCommand, timeline::SessionTimeline,
        };
        use std::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let owner = registry.get(CharacterId::Crazy).unwrap().clone();
        let initial = owner.memories().await.unwrap();
        let added = owner
            .edit_memories(
                initial,
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "forgotten live jasmine tea".into(),
                }],
            )
            .await
            .unwrap();
        let id = added.snapshot.records()[0].id;
        owner
            .edit_memories(added.snapshot, vec![MemoryCommand::Forget { id }])
            .await
            .unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(PausedQueryFixture {
            space: EmbeddingSpace::new("fixture", "real-owner-recall", "v1", 2).unwrap(),
            started: started.clone(),
            resume: resume.clone(),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(provider, 10).unwrap());
        let query = KnowledgeRetrieval::semantic(retriever);
        let handle = owner.clone();
        let task = tokio::spawn(async move {
            recall_from_character(&query, &handle, RecallSelector::Ids(vec![id]), "tea").await
        });
        tokio::time::timeout(std::time::Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        let snapshot = owner.memories().await.unwrap();
        owner
            .edit_memories(snapshot, vec![MemoryCommand::Restore { id }])
            .await
            .unwrap();
        resume.notify_one();
        let result = task.await.unwrap();
        assert!(matches!(
            result,
            Err(ExplicitRecallError::Retrieval(
                HistoricalMemorySelectionError::StaleRecall
            ))
        ));
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn lexical_selection_requires_no_embedding_provider() {
        let selected = KnowledgeRetrieval::lexical()
            .select(CharacterId::Crazy, &[fact()], "чай")
            .await
            .unwrap()
            .unwrap();
        assert!(selected.contains("ромашковый чай"));
        assert!(selected.contains(&Uuid::from_u128(14).to_string()));
    }

    #[tokio::test]
    async fn unavailable_semantic_provider_is_not_silent_lexical_success() {
        let fixture = Arc::new(UnavailableFixture {
            space: EmbeddingSpace::new("fixture", "unavailable", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 10).unwrap());
        let selected = KnowledgeRetrieval::semantic(retriever)
            .select(CharacterId::Crazy, &[fact()], "чай")
            .await;
        assert!(matches!(selected, Err(EmbeddingError::Transport(_))));
        assert_eq!(fixture.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn empty_semantic_corpus_does_not_contact_a_model() {
        let fixture = Arc::new(UnavailableFixture {
            space: EmbeddingSpace::new("fixture", "unavailable", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 10).unwrap());
        let selected = KnowledgeRetrieval::semantic(retriever)
            .select(CharacterId::Crazy, &[], "чай")
            .await
            .unwrap();
        assert!(selected.is_none());
        assert_eq!(fixture.calls.load(Ordering::Relaxed), 0);
    }
    #[tokio::test]
    async fn islands_have_separate_provenance_and_never_trigger_embedding() {
        use crate::memory::{MemoryIsland, MemoryKind};
        let mut island = fact();
        island.kind = MemoryKind::Island(MemoryIsland::Preferences);
        island.content = "Player chooses herbal tea".into();
        let fixture = Arc::new(UnavailableFixture {
            space: EmbeddingSpace::new("fixture", "unavailable", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retrieval = KnowledgeRetrieval::semantic(Arc::new(
            SemanticMemoryRetriever::new(fixture.clone(), 10).unwrap(),
        ));
        let selected = retrieval
            .select_with_islands(CharacterId::Crazy, &[], &[island], "tea")
            .await
            .unwrap()
            .unwrap();
        assert!(selected.contains("[MEMORY ISLANDS]"));
        assert!(selected.contains("preferences: Player chooses herbal tea"));
        assert!(!selected.contains("[LONG-TERM MEMORY]"));
        assert_eq!(fixture.calls.load(Ordering::Relaxed), 0);
    }
}
