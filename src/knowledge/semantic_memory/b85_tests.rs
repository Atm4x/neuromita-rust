use super::*;
use crate::timeline::{
    domain::{BranchId, MemoryPriority, SessionId},
    selection::TimelineSelection,
};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Notify, mpsc};

struct ControlledEmbeddings {
    space: EmbeddingSpace,
    block_first_document: AtomicBool,
    entered: mpsc::UnboundedSender<()>,
    release: Notify,
}

impl EmbeddingProvider for ControlledEmbeddings {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }
    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        _: String,
    ) -> super::super::embedding::EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            if purpose == EmbeddingPurpose::Document
                && self.block_first_document.swap(false, Ordering::SeqCst)
            {
                self.entered.send(()).unwrap();
                self.release.notified().await;
            }
            Embedding::new(vec![1.0, 0.5], &self.space)
        })
    }
}

fn memory(id: u128, text: &str) -> MemoryRecord {
    MemoryRecord {
        id: MemoryId(uuid::Uuid::from_u128(id)),
        priority: MemoryPriority::Normal,
        content: text.to_string(),
        kind: MemoryKind::Fact,
        state: MemoryState::Active,
        entities: vec![],
        protected: false,
        expires_at_unix_ms: None,
    }
}

fn stamp(
    character: CharacterId,
    selection: TimelineSelection,
    head: u64,
    epoch: u64,
) -> TimelineTurn {
    TimelineTurn {
        selection,
        context_head: head,
        character,
        context_epoch: epoch,
        trigger: None,
    }
}

fn provider(block: bool) -> (Arc<ControlledEmbeddings>, mpsc::UnboundedReceiver<()>) {
    let (entered, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(ControlledEmbeddings {
            space: EmbeddingSpace::new("test", "b85", "v1", 2).unwrap(),
            block_first_document: AtomicBool::new(block),
            entered,
            release: Notify::new(),
        }),
        receiver,
    )
}

async fn read(
    retriever: &SemanticMemoryRetriever,
    character: CharacterId,
    memories: &[MemoryRecord],
    turn: TimelineTurn,
) -> Result<RetrievedMemory, EmbeddingError> {
    retriever
        .retrieve_with_fusion_at(
            character,
            memories,
            "private",
            SemanticRetrievalFilter::all(),
            FusionMode::VectorOnly,
            Some(turn),
        )
        .await
}

#[tokio::test]
async fn b85_inflight_old_embedding_cannot_repopulate_purged_cache() {
    let (provider, mut entered) = provider(true);
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 10).unwrap());
    let selected = TimelineSelection {
        branch_id: BranchId::new(),
        generation: 1,
    };
    let old = stamp(CharacterId::Crazy, selected, 1, 0);
    let next = stamp(CharacterId::Crazy, selected, 2, 2);
    let task = {
        let retriever = retriever.clone();
        tokio::spawn(async move {
            read(
                &retriever,
                CharacterId::Crazy,
                &[memory(1, "private forgotten")],
                old,
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let cleared = read(&retriever, CharacterId::Crazy, &[], next)
        .await
        .unwrap();
    assert!(cleared.selected_ids.is_empty());
    assert!(retriever.cache.lock().unwrap().entries.is_empty());
    provider.release.notify_waiters();
    assert!(matches!(
        task.await.unwrap(),
        Err(EmbeddingError::StaleKnowledge)
    ));
    assert!(retriever.cache.lock().unwrap().entries.is_empty());
    assert!(matches!(
        read(
            &retriever,
            CharacterId::Crazy,
            &[memory(1, "private forgotten")],
            old
        )
        .await,
        Err(EmbeddingError::StaleKnowledge)
    ));
}

#[tokio::test]
async fn b85_memory_edit_evicts_old_content_without_affecting_other_character() {
    let (provider, _) = provider(false);
    let retriever = SemanticMemoryRetriever::new(provider, 16).unwrap();
    let selection = TimelineSelection {
        branch_id: BranchId::new(),
        generation: 1,
    };
    let first = stamp(CharacterId::Crazy, selection, 1, 0);
    let edited = stamp(CharacterId::Crazy, selection, 2, 0);
    let kind = stamp(CharacterId::Kind, selection, 1, 0);
    read(
        &retriever,
        CharacterId::Crazy,
        &[memory(1, "private previous")],
        first,
    )
    .await
    .unwrap();
    read(
        &retriever,
        CharacterId::Kind,
        &[memory(2, "private kind")],
        kind,
    )
    .await
    .unwrap();
    assert_eq!(retriever.cache.lock().unwrap().entries.len(), 2);
    let next = read(
        &retriever,
        CharacterId::Crazy,
        &[memory(1, "private edited")],
        edited,
    )
    .await
    .unwrap();
    assert_eq!(next.cost.cache_hits, 0);
    let cache = retriever.cache.lock().unwrap();
    assert_eq!(cache.entries.len(), 2);
    assert!(
        cache
            .entries
            .keys()
            .any(|key| key.character == CharacterId::Crazy && key.content == "private edited")
    );
    assert!(
        cache
            .entries
            .keys()
            .any(|key| key.character == CharacterId::Kind && key.content == "private kind")
    );
    assert!(
        !cache
            .entries
            .keys()
            .any(|key| key.content == "private previous")
    );
    drop(cache);
    assert!(matches!(
        read(
            &retriever,
            CharacterId::Crazy,
            &[memory(1, "private previous")],
            first
        )
        .await,
        Err(EmbeddingError::StaleKnowledge)
    ));
    assert_eq!(
        read(
            &retriever,
            CharacterId::Kind,
            &[memory(2, "private kind")],
            kind
        )
        .await
        .unwrap()
        .cost
        .cache_hits,
        1
    );
}

#[tokio::test]
async fn b85_explicit_clear_blocks_inflight_reinsertion() {
    let (provider, mut entered) = provider(true);
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 10).unwrap());
    let task = {
        let retriever = retriever.clone();
        tokio::spawn(async move {
            retriever
                .retrieve(CharacterId::Crazy, &[memory(1, "private old")], "private")
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.recv())
        .await
        .unwrap()
        .unwrap();
    retriever.clear_cache();
    provider.release.notify_waiters();
    assert!(matches!(
        task.await.unwrap(),
        Err(EmbeddingError::StaleKnowledge)
    ));
    assert!(retriever.cache.lock().unwrap().entries.is_empty());
}

#[cfg(feature = "sqlite-system")]
async fn sqlite_external_event_evicts_on_next_authoritative_snapshot(mode: u8) {
    use crate::timeline::{SessionTimeline, TimelineConfig};
    let db = std::env::temp_dir().join(format!("b85-sql-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut first = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let input = crate::identity::EventId::new();
    let other = crate::identity::EventId::new();
    first
        .publish_player_message(CharacterId::Crazy, input, Arc::from("source"))
        .unwrap();
    first
        .publish_player_message(CharacterId::Kind, other, Arc::from("unrelated"))
        .unwrap();
    if mode == 1 {
        first
            .change_character_knowledge(
                first.selection(),
                CharacterId::Crazy,
                crate::identity::EventId::new(),
                false,
                vec![input],
            )
            .unwrap();
    }
    let start = first
        .capture_turn(CharacterId::Crazy, None, first.anchor())
        .unwrap();
    let old_known = mode != 1;
    assert_eq!(
        first
            .knows_event_at(CharacterId::Crazy, input, None)
            .unwrap(),
        old_known
    );
    let mut foreign = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let (provider, _) = provider(false);
    let retriever = SemanticMemoryRetriever::new(provider, 16).unwrap();
    read(
        &retriever,
        CharacterId::Crazy,
        &[memory(1, "private old")],
        start,
    )
    .await
    .unwrap();
    assert_eq!(retriever.cache.lock().unwrap().entries.len(), 1);
    match mode {
        0 => {
            foreign
                .change_character_knowledge(
                    foreign.selection(),
                    CharacterId::Crazy,
                    crate::identity::EventId::new(),
                    false,
                    vec![input],
                )
                .unwrap();
        }
        1 => {
            foreign
                .change_character_knowledge(
                    foreign.selection(),
                    CharacterId::Crazy,
                    crate::identity::EventId::new(),
                    true,
                    vec![input],
                )
                .unwrap();
        }
        2 => {
            foreign
                .reset_character_context(
                    foreign.selection(),
                    CharacterId::Crazy,
                    crate::identity::EventId::new(),
                )
                .unwrap();
        }
        _ => unreachable!(),
    }
    let current = first
        .capture_turn(CharacterId::Crazy, None, first.anchor())
        .unwrap();
    assert!(current.context_head > start.context_head);
    assert!(current.context_epoch > start.context_epoch);
    read(&retriever, CharacterId::Crazy, &[], current)
        .await
        .unwrap();
    assert!(retriever.cache.lock().unwrap().entries.is_empty());
    assert!(matches!(
        read(
            &retriever,
            CharacterId::Crazy,
            &[memory(1, "private old")],
            start
        )
        .await,
        Err(EmbeddingError::StaleKnowledge)
    ));
    assert_eq!(
        first
            .knows_event_at(CharacterId::Crazy, input, Some(start.context_head))
            .unwrap(),
        old_known
    );
    assert_eq!(
        first
            .knows_event_at(CharacterId::Crazy, input, None)
            .unwrap(),
        mode == 1
    );
    assert!(
        first
            .knows_event_at(CharacterId::Kind, other, None)
            .unwrap()
    );
    drop((first, foreign, retriever));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b85_sqlite_foreign_forget_evicts_on_authoritative_snapshot() {
    sqlite_external_event_evicts_on_next_authoritative_snapshot(0).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b85_sqlite_foreign_reveal_evicts_on_authoritative_snapshot() {
    sqlite_external_event_evicts_on_next_authoritative_snapshot(1).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn b85_sqlite_foreign_history_reset_evicts_on_authoritative_snapshot() {
    sqlite_external_event_evicts_on_next_authoritative_snapshot(2).await;
}
