use super::*;
use crate::{
    character::memory_access::{edit_memory_snapshot, read_memory_snapshot},
    knowledge::{
        embedding::{Embedding, EmbeddingFuture, EmbeddingPurpose},
        semantic_memory::SemanticRetrievalFilter,
    },
    memory::MemoryCommand,
    timeline::domain::{BranchId, MemoryPriority, SessionId},
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct LocalEmbeddings {
    space: EmbeddingSpace,
    document_calls: AtomicUsize,
}

impl LocalEmbeddings {
    fn new() -> Self {
        Self {
            space: EmbeddingSpace::new("local", "b86", "v1", 2).unwrap(),
            document_calls: AtomicUsize::new(0),
        }
    }
}

impl EmbeddingProvider for LocalEmbeddings {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    fn embed(&self, purpose: EmbeddingPurpose, _text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            if purpose == EmbeddingPurpose::Document {
                self.document_calls.fetch_add(1, Ordering::SeqCst);
            }
            Embedding::new(vec![1.0, 0.5], &self.space)
        })
    }
}

fn insert_private_memory(timeline: &mut SessionTimeline) -> crate::memory::MemoryId {
    let before = read_memory_snapshot(timeline, CharacterId::Crazy).unwrap();
    let changed = edit_memory_snapshot(
        timeline,
        CharacterId::Crazy,
        &before,
        &[MemoryCommand::Add {
            priority: MemoryPriority::High,
            content: "a private document".into(),
        }],
    )
    .unwrap();
    changed.snapshot.records()[0].id
}

async fn warm(
    timeline: &Arc<Mutex<SessionTimeline>>,
    retriever: &SemanticMemoryRetriever,
) -> crate::timeline::selection::TimelineTurn {
    let (turn, records) = {
        let timeline = timeline.lock().unwrap();
        let turn = timeline
            .capture_turn(CharacterId::Crazy, None, timeline.anchor())
            .unwrap();
        let records = read_memory_snapshot(&timeline, CharacterId::Crazy).unwrap();
        (turn, records.records().to_vec())
    };
    retriever
        .retrieve_with_fusion_at(
            CharacterId::Crazy,
            &records,
            "private",
            SemanticRetrievalFilter::all(),
            crate::knowledge::retrieval_fusion::FusionMode::VectorOnly,
            Some(turn),
        )
        .await
        .unwrap();
    turn
}

async fn wait_until_reconciled(
    knowledge: &KnowledgeRetrieval,
    timeline: &Arc<Mutex<SessionTimeline>>,
    old: crate::timeline::selection::TimelineTurn,
) {
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let turn = {
                let timeline = timeline.lock().unwrap();
                let turn = timeline
                    .capture_turn(CharacterId::Crazy, None, timeline.anchor())
                    .unwrap();
                turn
            };
            if turn != old && !knowledge.needs_authoritative_refresh(CharacterId::Crazy, turn) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn b86_local_context_reset_watcher_reconciles_cached_memory() {
    let mut initial = SessionTimeline::new();
    insert_private_memory(&mut initial);
    let timeline = Arc::new(Mutex::new(initial));
    let provider = Arc::new(LocalEmbeddings::new());
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 16).unwrap());
    let knowledge = Arc::new(KnowledgeRetrieval::semantic(retriever.clone()));
    let old = warm(&timeline, &retriever).await;
    let (memory, context) = {
        let timeline = timeline.lock().unwrap();
        (
            timeline.observe_memory_changes(CharacterId::Crazy),
            timeline.observe_context_epoch(CharacterId::Crazy),
        )
    };
    let stop = CancellationToken::new();
    let task = tokio::spawn(run_semantic_knowledge_reconciler(
        timeline.clone(),
        knowledge.clone(),
        CharacterId::Crazy,
        memory,
        context,
        stop.clone(),
    ));
    {
        let mut timeline = timeline.lock().unwrap();
        let selected = timeline.selection();
        timeline
            .reset_character_context(selected, CharacterId::Crazy, EventId::new())
            .unwrap();
    }
    wait_until_reconciled(&knowledge, &timeline, old).await;
    let current = {
        let timeline = timeline.lock().unwrap();
        timeline
            .capture_turn(CharacterId::Crazy, None, timeline.anchor())
            .unwrap()
    };
    assert!(current.context_epoch > old.context_epoch);
    assert_eq!(provider.document_calls.load(Ordering::SeqCst), 1);
    assert!(retriever.needs_authoritative_refresh(CharacterId::Crazy, old));
    stop.cancel();
    task.await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b86_foreign_sqlite_memory_delete_evicted_while_owner_idle() {
    let db = std::env::temp_dir().join(format!("b86-idle-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut first = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let memory_id = insert_private_memory(&mut first);
    let timeline = Arc::new(Mutex::new(first));
    let provider = Arc::new(LocalEmbeddings::new());
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 16).unwrap());
    let knowledge = Arc::new(KnowledgeRetrieval::semantic(retriever.clone()));
    let old = warm(&timeline, &retriever).await;
    let (memory, context) = {
        let timeline = timeline.lock().unwrap();
        (
            timeline.observe_memory_changes(CharacterId::Crazy),
            timeline.observe_context_epoch(CharacterId::Crazy),
        )
    };
    let stop = CancellationToken::new();
    let task = tokio::spawn(run_semantic_knowledge_reconciler(
        timeline.clone(),
        knowledge.clone(),
        CharacterId::Crazy,
        memory,
        context,
        stop.clone(),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut foreign = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let before = read_memory_snapshot(&foreign, CharacterId::Crazy).unwrap();
    edit_memory_snapshot(
        &mut foreign,
        CharacterId::Crazy,
        &before,
        &[MemoryCommand::SoftDelete { id: memory_id }],
    )
    .unwrap();
    wait_until_reconciled(&knowledge, &timeline, old).await;
    let present = {
        let timeline = timeline.lock().unwrap();
        let snapshot = read_memory_snapshot(&timeline, CharacterId::Crazy).unwrap();
        assert_eq!(snapshot.records().len(), 1);
        assert_eq!(
            snapshot.records()[0].state,
            crate::memory::MemoryState::Deleted
        );
        let page = crate::presentation::MemoryListPage::from_snapshot(
            snapshot,
            crate::presentation::MemoryListQuery::default(),
        )
        .unwrap();
        assert_eq!(page.summary().deleted, 1);
        assert_eq!(page.summary().active, 0);
        timeline
            .capture_turn(CharacterId::Crazy, None, timeline.anchor())
            .unwrap()
    };
    assert!(present.context_head > old.context_head);
    assert_eq!(provider.document_calls.load(Ordering::SeqCst), 1);
    assert!(retriever.needs_authoritative_refresh(CharacterId::Crazy, old));
    stop.cancel();
    task.await.unwrap();
    drop((foreign, timeline, knowledge, retriever));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
async fn b86_foreign_control_refreshes_idle_cache(mode: u8) {
    let db = std::env::temp_dir().join(format!("b86-foreign-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut first = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let input = EventId::new();
    first
        .publish_player_message(CharacterId::Crazy, input, Arc::from("source"))
        .unwrap();
    let unrelated = EventId::new();
    first
        .publish_player_message(CharacterId::Kind, unrelated, Arc::from("independent"))
        .unwrap();
    if mode == 1 {
        let selected = first.selection();
        first
            .change_character_knowledge(
                selected,
                CharacterId::Crazy,
                EventId::new(),
                false,
                vec![input],
            )
            .unwrap();
    }
    insert_private_memory(&mut first);
    let prior_known = first
        .knows_event_at(CharacterId::Crazy, input, None)
        .unwrap();
    let timeline = Arc::new(Mutex::new(first));
    let provider = Arc::new(LocalEmbeddings::new());
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 16).unwrap());
    let knowledge = Arc::new(KnowledgeRetrieval::semantic(retriever.clone()));
    let old = warm(&timeline, &retriever).await;
    let (memory, context) = {
        let timeline = timeline.lock().unwrap();
        (
            timeline.observe_memory_changes(CharacterId::Crazy),
            timeline.observe_context_epoch(CharacterId::Crazy),
        )
    };
    let stop = CancellationToken::new();
    let task = tokio::spawn(run_semantic_knowledge_reconciler(
        timeline.clone(),
        knowledge.clone(),
        CharacterId::Crazy,
        memory,
        context,
        stop.clone(),
    ));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut foreign = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let selected = foreign.selection();
    match mode {
        0 => {
            foreign
                .change_character_knowledge(
                    selected,
                    CharacterId::Crazy,
                    EventId::new(),
                    false,
                    vec![input],
                )
                .unwrap();
        }
        1 => {
            foreign
                .change_character_knowledge(
                    selected,
                    CharacterId::Crazy,
                    EventId::new(),
                    true,
                    vec![input],
                )
                .unwrap();
        }
        2 => {
            foreign
                .reset_character_context(selected, CharacterId::Crazy, EventId::new())
                .unwrap();
        }
        _ => unreachable!(),
    }
    wait_until_reconciled(&knowledge, &timeline, old).await;
    let current = {
        let owner = timeline.lock().unwrap();
        assert_eq!(
            owner
                .knows_event_at(CharacterId::Crazy, input, Some(old.context_head))
                .unwrap(),
            prior_known
        );
        assert_eq!(
            owner
                .knows_event_at(CharacterId::Crazy, input, None)
                .unwrap(),
            mode == 1
        );
        assert!(
            owner
                .knows_event_at(CharacterId::Kind, unrelated, None)
                .unwrap()
        );
        owner
            .capture_turn(CharacterId::Crazy, None, owner.anchor())
            .unwrap()
    };
    assert!(current.context_epoch > old.context_epoch);
    assert_eq!(provider.document_calls.load(Ordering::SeqCst), 1);
    assert!(retriever.needs_authoritative_refresh(CharacterId::Crazy, old));
    stop.cancel();
    task.await.unwrap();
    drop((foreign, timeline, knowledge, retriever));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b86_foreign_sqlite_forget_refreshes_cache_while_idle() {
    b86_foreign_control_refreshes_idle_cache(0).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b86_foreign_sqlite_reveal_refreshes_cache_while_idle() {
    b86_foreign_control_refreshes_idle_cache(1).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b86_foreign_sqlite_history_reset_refreshes_cache_while_idle() {
    b86_foreign_control_refreshes_idle_cache(2).await;
}
