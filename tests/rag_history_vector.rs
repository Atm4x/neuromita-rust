use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use neuromita::{
    character::CharacterId,
    knowledge::{
        embedding::{
            Embedding, EmbeddingError, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose,
            EmbeddingSpace,
        },
        history_retrieval::{
            HistorySearchConfig, HistorySearchError, HistorySearchStatus,
            search_archived_history_semantic, search_history, search_history_semantic,
        },
        history_window::HistoryWindow,
        retrieval_fusion::{FusionMode, RetrievalMethod},
        semantic_memory::SemanticMemoryRetriever,
    },
    memory::{MemoryKind, MemoryState},
    timeline::{
        domain::{
            BranchId, EventDraft, EventKind, MemoryId, MemoryPriority, MemoryRecord, SessionId,
            TimelineRepository, TimelineStore,
        },
        view::TimelineMessageQueries,
    },
};
use serde_json::json;

#[derive(Clone, Copy)]
enum Fault {
    None,
    Down,
    WrongQuerySpace,
    WrongDocumentSpace,
    WrongDimensions,
    ShortBatch,
}

struct Fixture {
    space: EmbeddingSpace,
    calls: AtomicUsize,
    document_calls: AtomicUsize,
    fault: Fault,
}

impl Fixture {
    fn new(model: &str, fault: Fault) -> Self {
        Self {
            space: EmbeddingSpace::new("fixture", model, "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
            document_calls: AtomicUsize::new(0),
            fault,
        }
    }
    fn vector(&self, text: &str, fault: Fault) -> Embedding {
        let mut space = self.space.clone();
        match fault {
            Fault::WrongQuerySpace | Fault::WrongDocumentSpace => space.model = "foreign".into(),
            Fault::WrongDimensions => space.dimensions = 3,
            _ => {}
        }
        let coords = if text.contains("tea") || text.contains("warm") {
            vec![1.0, 0.0]
        } else {
            vec![0.0, 1.0]
        };
        let coords = if space.dimensions == 3 {
            vec![1.0, 0.0, 0.0]
        } else {
            coords
        };
        Embedding::new(coords, &space).unwrap()
    }
}

impl EmbeddingProvider for Fixture {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }
    fn embed(&self, _purpose: EmbeddingPurpose, text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if matches!(self.fault, Fault::Down) {
                return Err(EmbeddingError::Transport("fixture down".into()));
            }
            Ok(self.vector(
                &text,
                if matches!(self.fault, Fault::WrongQuerySpace | Fault::WrongDimensions) {
                    self.fault
                } else {
                    Fault::None
                },
            ))
        })
    }
    fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
        Box::pin(async move {
            self.document_calls.fetch_add(1, Ordering::SeqCst);
            let mut embeddings = texts
                .iter()
                .map(|text| {
                    self.vector(
                        text,
                        if matches!(self.fault, Fault::WrongDocumentSpace) {
                            self.fault
                        } else {
                            Fault::None
                        },
                    )
                })
                .collect::<Vec<_>>();
            if matches!(self.fault, Fault::ShortBatch) {
                embeddings.pop();
            }
            Ok(embeddings)
        })
    }
}

fn messages(
    repo: &dyn TimelineRepository,
    branch: BranchId,
    character: CharacterId,
) -> neuromita::timeline::view::MitaView {
    TimelineMessageQueries::capture(repo, branch, character, None).unwrap()
}

fn fixture_timeline() -> (TimelineStore, BranchId) {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    (repo, branch)
}

fn append(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    characters: &[CharacterId],
    text: &str,
) -> neuromita::timeline::EventId {
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft.observers.extend(characters.iter().copied());
    draft.payload.insert("content".into(), json!(text));
    let id = draft.id;
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(branch, head, &id.0.to_string(), json!({}), vec![draft])
        .unwrap();
    id
}

fn reset(repo: &mut dyn TimelineRepository, branch: BranchId, character: CharacterId) {
    let mut draft = EventDraft::new(EventKind::CharacterContextReset);
    draft.observers.insert(character);
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(
        branch,
        head,
        &draft.id.0.to_string(),
        json!({}),
        vec![draft],
    )
    .unwrap();
}

fn setup(fault: Fault) -> (Arc<Fixture>, SemanticMemoryRetriever) {
    let fixture = Arc::new(Fixture::new("model-one", fault));
    let retriever = SemanticMemoryRetriever::new(fixture.clone(), 32).unwrap();
    (fixture, retriever)
}

#[tokio::test]
async fn vector_history_retrieves_paraphrase_and_reuses_shared_model_scoped_cache() {
    let (fixture, retriever) = setup(Fault::None);
    let (mut repo, branch) = fixture_timeline();
    let target = append(&mut repo, branch, &[CharacterId::Crazy], "tea in kitchen");
    append(&mut repo, branch, &[CharacterId::Crazy], "oil painting");
    let snapshot = messages(&repo, branch, CharacterId::Crazy);
    assert_eq!(
        search_history(
            CharacterId::Crazy,
            &snapshot,
            "warm",
            HistorySearchConfig::default()
        )
        .unwrap()
        .status,
        HistorySearchStatus::NoMatches
    );
    let result = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &snapshot,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.8),
    )
    .await
    .unwrap();
    assert_eq!(result.status, HistorySearchStatus::Selected);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key.id, target.0.to_string());
    assert_eq!(result.hits[0].evidence[0].method, RetrievalMethod::Vector);
    assert_eq!(result.hits[0].metadata["branch_id"], branch.0.to_string());
    assert_eq!(result.embedding_space.unwrap(), fixture.space);
    assert_eq!(result.cost.unwrap().cache_hits, 0);
    let repeated = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &snapshot,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(repeated.cost.unwrap().cache_hits, 2);
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    let memory = MemoryRecord {
        id: MemoryId(target.0),
        priority: MemoryPriority::Normal,
        content: "tea in kitchen".into(),
        kind: MemoryKind::Fact,
        state: MemoryState::Active,
        entities: Vec::new(),
        protected: false,
        expires_at_unix_ms: None,
    };
    let memory_result = retriever
        .retrieve(CharacterId::Crazy, &[memory], "warm")
        .await
        .unwrap();
    assert_eq!(memory_result.cost.cache_hits, 0);
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn vector_corpus_states_and_fusion_filter_are_explicit() {
    let (fixture, retriever) = setup(Fault::None);
    let (mut repo, branch) = fixture_timeline();
    let initial = messages(&repo, branch, CharacterId::Crazy);
    let empty = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &initial,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(empty.status, HistorySearchStatus::EmptyCorpus);
    assert_eq!(empty.cost.unwrap().embedding_requests, 0);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    append(&mut repo, branch, &[CharacterId::Crazy], "oil painting");
    let snapshot = messages(&repo, branch, CharacterId::Crazy);
    let no_matches = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &snapshot,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.8),
    )
    .await
    .unwrap();
    assert_eq!(no_matches.status, HistorySearchStatus::NoMatches);
    let union = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &snapshot,
        "painting",
        HistorySearchConfig::default(),
        FusionMode::Union,
        Some(0.8),
    )
    .await
    .unwrap();
    assert_eq!(union.hits.len(), 1);
    assert_eq!(union.hits[0].evidence.len(), 2);
    assert!(
        union.hits[0]
            .evidence
            .iter()
            .any(|e| e.method == RetrievalMethod::Vector)
    );
    assert!(
        union.hits[0]
            .evidence
            .iter()
            .any(|e| e.method == RetrievalMethod::Keyword)
    );
    assert!(matches!(
        search_history_semantic(
            &retriever,
            CharacterId::Crazy,
            &snapshot,
            "warm",
            HistorySearchConfig::default(),
            FusionMode::VectorOnly,
            Some(f64::NAN),
        )
        .await,
        Err(HistorySearchError::Configuration(
            "invalid cosine threshold"
        ))
    ));
}

#[tokio::test]
async fn model_space_and_provider_failures_are_not_keyword_fallbacks_or_cache_entries() {
    let (mut repo, branch) = fixture_timeline();
    append(&mut repo, branch, &[CharacterId::Crazy], "tea in kitchen");
    let view = messages(&repo, branch, CharacterId::Crazy);
    for fault in [
        Fault::Down,
        Fault::WrongQuerySpace,
        Fault::WrongDocumentSpace,
        Fault::WrongDimensions,
        Fault::ShortBatch,
    ] {
        let (fixture, retriever) = setup(fault);
        let error = search_history_semantic(
            &retriever,
            CharacterId::Crazy,
            &view,
            "tea",
            HistorySearchConfig::default(),
            FusionMode::Union,
            None,
        )
        .await
        .unwrap_err();
        match fault {
            Fault::Down => assert!(matches!(
                error,
                HistorySearchError::Embedding(EmbeddingError::Transport(_))
            )),
            Fault::WrongQuerySpace | Fault::WrongDocumentSpace => assert!(matches!(
                error,
                HistorySearchError::Embedding(EmbeddingError::ModelSpaceMismatch { .. })
            )),
            Fault::WrongDimensions => assert!(matches!(
                error,
                HistorySearchError::Embedding(EmbeddingError::ModelSpaceMismatch { .. })
            )),
            Fault::ShortBatch => assert!(matches!(
                error,
                HistorySearchError::Embedding(EmbeddingError::Response(_))
            )),
            Fault::None => unreachable!(),
        }
        assert_eq!(retriever.preview_cache_clear().total_entries, 0);
        assert!(fixture.calls.load(Ordering::SeqCst) >= 1);
    }
}

#[tokio::test]
async fn history_vector_never_reads_other_character_and_rechecks_revision_reset() {
    let (_, retriever) = setup(Fault::None);
    let (mut repo, branch) = fixture_timeline();
    append(&mut repo, branch, &[CharacterId::Crazy], "private tea");
    let both = append(
        &mut repo,
        branch,
        &[CharacterId::Crazy, CharacterId::Kind],
        "shared tea",
    );
    let crazy = messages(&repo, branch, CharacterId::Crazy);
    let kind = messages(&repo, branch, CharacterId::Kind);
    assert!(matches!(
        search_history_semantic(
            &retriever,
            CharacterId::Kind,
            &crazy,
            "warm",
            HistorySearchConfig::default(),
            FusionMode::VectorOnly,
            None,
        )
        .await,
        Err(HistorySearchError::WrongCharacter)
    ));
    let kind_hits = search_history_semantic(
        &retriever,
        CharacterId::Kind,
        &kind,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.9),
    )
    .await
    .unwrap();
    assert_eq!(kind_hits.hits.len(), 1);
    assert_eq!(kind_hits.hits[0].key.id, both.0.to_string());
    reset(&mut repo, branch, CharacterId::Crazy);
    let current = messages(&repo, branch, CharacterId::Crazy);
    let changed = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &current,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(changed.status, HistorySearchStatus::EmptyCorpus);
    let kind_again = search_history_semantic(
        &retriever,
        CharacterId::Kind,
        &messages(&repo, branch, CharacterId::Kind),
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(kind_again.hits.len(), 1);
    assert_eq!(kind_again.cost.unwrap().cache_hits, 1);
}

#[tokio::test]
async fn history_revision_reindexes_only_changed_visible_document_and_confirmed_clear_invalidates_cache()
 {
    let (fixture, retriever) = setup(Fault::None);
    let (mut repo, branch) = fixture_timeline();
    let event = append(&mut repo, branch, &[CharacterId::Crazy], "tea in kitchen");
    let first_view = messages(&repo, branch, CharacterId::Crazy);
    let first = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &first_view,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.9),
    )
    .await
    .unwrap();
    assert_eq!(first.hits.len(), 1);
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 1);

    let mut revise = EventDraft::new(EventKind::MessageRevised);
    revise.observers.insert(CharacterId::Crazy);
    revise
        .payload
        .insert("event_id".into(), json!(event.0.to_string()));
    revise
        .payload
        .insert("message".into(), json!({"content": "oil painting"}));
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(
        branch,
        head,
        &revise.id.0.to_string(),
        json!({}),
        vec![revise],
    )
    .unwrap();
    let changed_view = messages(&repo, branch, CharacterId::Crazy);
    let changed = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &changed_view,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.9),
    )
    .await
    .unwrap();
    assert_eq!(changed.status, HistorySearchStatus::NoMatches);
    assert_eq!(changed.cost.unwrap().cache_hits, 0);
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 2);

    let preview = retriever.preview_cache_clear();
    assert_eq!(preview.total_entries, 2);
    assert_eq!(retriever.clear_cache_confirmed(&preview, true).unwrap(), 2);
    let rerun = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &changed_view,
        "oil",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(rerun.cost.unwrap().cache_hits, 0);
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 3);
    let historical = search_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &first_view,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(historical.hits.len(), 1);
    assert_eq!(historical.hits[0].content, "tea in kitchen");
}

#[tokio::test]
async fn vector_archived_window_only_embeds_excluded_history_and_never_active() {
    let (fixture, retriever) = setup(Fault::None);
    let (mut repo, branch) = fixture_timeline();
    let old = append(&mut repo, branch, &[CharacterId::Crazy], "tea in kitchen");
    let active = append(
        &mut repo,
        branch,
        &[CharacterId::Crazy],
        "tea in current window",
    );
    let view = messages(&repo, branch, CharacterId::Crazy);
    let window = HistoryWindow::capture(&view, None, 1);
    assert!(window.is_archived(old));
    assert!(window.is_active(active));
    let result = search_archived_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &view,
        &window,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.8),
    )
    .await
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].key.id, old.0.to_string());
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 1);
    let repeated = search_archived_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &view,
        &window,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::Union,
        Some(0.8),
    )
    .await
    .unwrap();
    assert!(
        repeated
            .hits
            .iter()
            .all(|hit| hit.key.id == old.0.to_string())
    );
    assert_eq!(fixture.document_calls.load(Ordering::SeqCst), 1);
    let focused = HistoryWindow::capture(&view, Some(old), 1);
    let none = search_archived_history_semantic(
        &retriever,
        CharacterId::Crazy,
        &view,
        &focused,
        "warm",
        HistorySearchConfig::default(),
        FusionMode::VectorOnly,
        Some(0.8),
    )
    .await
    .unwrap();
    assert_eq!(none.status, HistorySearchStatus::EmptyCorpus);
}
