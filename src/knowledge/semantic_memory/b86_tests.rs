use super::*;
use crate::timeline::{domain::BranchId, selection::TimelineSelection};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Notify, mpsc};

struct ControlledProvider {
    space: EmbeddingSpace,
    pause_first: AtomicBool,
    entered: mpsc::UnboundedSender<()>,
    released: Notify,
    documents: AtomicUsize,
}

impl EmbeddingProvider for ControlledProvider {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }

    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        _text: String,
    ) -> super::super::embedding::EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            if purpose == EmbeddingPurpose::Document {
                self.documents.fetch_add(1, Ordering::SeqCst);
                if self.pause_first.swap(false, Ordering::SeqCst) {
                    self.entered.send(()).unwrap();
                    self.released.notified().await;
                }
            }
            Embedding::new(vec![1.0, 0.5], &self.space)
        })
    }
}

fn fixture(block: bool) -> (Arc<ControlledProvider>, mpsc::UnboundedReceiver<()>) {
    let (entered, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(ControlledProvider {
            space: EmbeddingSpace::new("local", "b86", "v1", 2).unwrap(),
            pause_first: AtomicBool::new(block),
            entered,
            released: Notify::new(),
            documents: AtomicUsize::new(0),
        }),
        receiver,
    )
}

fn sample() -> MemoryRecord {
    MemoryRecord {
        id: MemoryId(uuid::Uuid::from_u128(100)),
        priority: crate::timeline::domain::MemoryPriority::Normal,
        content: "private semantic content".into(),
        kind: MemoryKind::Fact,
        state: MemoryState::Active,
        entities: vec![],
        protected: false,
        expires_at_unix_ms: None,
    }
}

fn turn(
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

async fn retrieve(
    retriever: &SemanticMemoryRetriever,
    turn: TimelineTurn,
) -> Result<RetrievedMemory, EmbeddingError> {
    retriever
        .retrieve_with_fusion_at(
            turn.character,
            &[sample()],
            "private",
            SemanticRetrievalFilter::all(),
            FusionMode::VectorOnly,
            Some(turn),
        )
        .await
}

#[tokio::test]
async fn b86_identical_authoritative_snapshot_allows_concurrent_inflight_retrieval() {
    let (provider, mut entered) = fixture(true);
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 16).unwrap());
    let selected = TimelineSelection {
        branch_id: BranchId::new(),
        generation: 1,
    };
    let pinned = turn(CharacterId::Crazy, selected, 1, 0);
    let pending = {
        let retriever = retriever.clone();
        tokio::spawn(async move { retrieve(&retriever, pinned).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let parallel = retrieve(&retriever, pinned).await.unwrap();
    assert_eq!(parallel.selected_ids, vec![sample().id]);
    provider.released.notify_waiters();
    let first = pending.await.unwrap().unwrap();
    assert_eq!(first.selected_ids, parallel.selected_ids);
    assert_eq!(retriever.cache.lock().unwrap().entries.len(), 1);
}

#[tokio::test]
async fn b86_identical_snapshot_reuses_existing_embedding_without_barrier_churn() {
    let (provider, _) = fixture(false);
    let retriever = SemanticMemoryRetriever::new(provider.clone(), 16).unwrap();
    let pinned = turn(
        CharacterId::Crazy,
        TimelineSelection {
            branch_id: BranchId::new(),
            generation: 1,
        },
        1,
        0,
    );
    retrieve(&retriever, pinned).await.unwrap();
    let before = retriever.cache.lock().unwrap().revisions[&CharacterId::Crazy];
    let second = retrieve(&retriever, pinned).await.unwrap();
    assert_eq!(second.cost.cache_hits, 1);
    assert_eq!(provider.documents.load(Ordering::SeqCst), 1);
    assert_eq!(
        retriever.cache.lock().unwrap().revisions[&CharacterId::Crazy],
        before
    );
}

#[tokio::test]
async fn b86_authoritative_idle_refresh_cancels_inflight_and_preserves_other_character() {
    let (provider, mut entered) = fixture(false);
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 16).unwrap());
    let selection = TimelineSelection {
        branch_id: BranchId::new(),
        generation: 1,
    };
    let old = turn(CharacterId::Crazy, selection, 1, 0);
    let unaffected = turn(CharacterId::Kind, selection, 1, 0);
    retrieve(&retriever, unaffected).await.unwrap();
    provider.pause_first.store(true, Ordering::SeqCst);
    let pending = {
        let retriever = retriever.clone();
        tokio::spawn(async move { retrieve(&retriever, old).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.recv())
        .await
        .unwrap()
        .unwrap();
    let current = turn(CharacterId::Crazy, selection, 2, 2);
    assert!(retriever.needs_authoritative_refresh(CharacterId::Crazy, current));
    retriever
        .refresh_authoritative(CharacterId::Crazy, current, &[])
        .unwrap();
    assert!(!retriever.needs_authoritative_refresh(CharacterId::Crazy, current));
    provider.released.notify_waiters();
    assert!(matches!(
        pending.await.unwrap(),
        Err(EmbeddingError::StaleKnowledge)
    ));
    assert!(matches!(
        retrieve(&retriever, old).await,
        Err(EmbeddingError::StaleKnowledge)
    ));
    assert_eq!(
        retrieve(&retriever, unaffected)
            .await
            .unwrap()
            .cost
            .cache_hits,
        1
    );
}

#[tokio::test]
async fn b86_authoritative_refresh_prunes_legacy_unscoped_cache_entries() {
    let (provider, _) = fixture(false);
    let retriever = SemanticMemoryRetriever::new(provider, 16).unwrap();
    retriever
        .retrieve(CharacterId::Crazy, &[sample()], "private")
        .await
        .unwrap();
    assert_eq!(retriever.cache.lock().unwrap().entries.len(), 1);
    let pinned = turn(
        CharacterId::Crazy,
        TimelineSelection {
            branch_id: BranchId::new(),
            generation: 1,
        },
        2,
        1,
    );
    assert!(retriever.needs_authoritative_refresh(CharacterId::Crazy, pinned));
    retriever
        .refresh_authoritative(CharacterId::Crazy, pinned, &[])
        .unwrap();
    assert!(retriever.cache.lock().unwrap().entries.is_empty());
}
