use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use neuromita::{
    UserInput,
    api::{
        ChatApi, HistorySearchApi, HistorySearchApiError, KnowledgeCacheApi, KnowledgeCacheApiError,
    },
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::{
        KnowledgeRetrieval,
        embedding::{
            Embedding, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace,
        },
        history_retrieval::{HistorySearchError, HistorySearchStatus},
        retrieval_fusion::FusionMode,
        semantic_memory::{EmbeddingCacheMaintenanceError, SemanticMemoryRetriever},
    },
};

static API_RUNTIME_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct DisabledProvider;
impl GenerationProvider for DisabledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, _: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async { Err(GenerationError::Transport("unused".into())) })
    }
}

fn application_config(knowledge: KnowledgeRetrieval) -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture",
            "unused-key",
            GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let capacity = |size| NonZeroUsize::new(size).unwrap();
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([Arc::new(DisabledProvider) as Arc<dyn GenerationProvider>])
            .unwrap(),
        RuntimeLimits {
            request_queue: capacity(4),
            discovery_buffer: capacity(8),
            character_commands: capacity(16),
            presenter_updates: capacity(8),
            retained_finished: capacity(8),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.knowledge = knowledge;
    config
}

#[tokio::test]
async fn local_history_api_reads_live_scoped_timeline_and_cannot_generate() {
    let _single_application = API_RUNTIME_TEST.lock().await;
    let app = ApplicationLifecycle::start(application_config(KnowledgeRetrieval::lexical()))
        .await
        .unwrap();
    let before = HistorySearchApi::inspect(CharacterId::Crazy, "jasmine")
        .await
        .unwrap();
    assert_eq!(before.status, HistorySearchStatus::EmptyCorpus);

    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("I prefer jasmine green tea".into()).unwrap(),
    )
    .unwrap();
    let crazy = HistorySearchApi::inspect(CharacterId::Crazy, "jasmine green")
        .await
        .unwrap();
    assert_eq!(crazy.status, HistorySearchStatus::Selected);
    assert_eq!(crazy.hits[0].key.id, event.0.to_string());
    assert_eq!(crazy.hits[0].content, "I prefer jasmine green tea");
    assert_eq!(crazy.hits[0].metadata["role"], "user");
    assert_eq!(crazy.hits[0].metadata["through"], crazy.through.to_string());
    let other = HistorySearchApi::inspect(CharacterId::Kind, "jasmine")
        .await
        .unwrap();
    assert_eq!(other.status, HistorySearchStatus::EmptyCorpus);
    assert!(other.hits.is_empty());

    app.shutdown().await.unwrap();
    assert!(matches!(
        HistorySearchApi::inspect(CharacterId::Crazy, "jasmine").await,
        Err(HistorySearchApiError::Unavailable)
    ));
}

struct PausedEmbedding {
    space: EmbeddingSpace,
    first_query_started: Arc<tokio::sync::Notify>,
    first_query_resume: Arc<tokio::sync::Notify>,
    pause_once: Mutex<bool>,
}

impl PausedEmbedding {
    fn new() -> Self {
        Self {
            space: EmbeddingSpace::new("fixture", "paused", "v1", 2).unwrap(),
            first_query_started: Arc::new(tokio::sync::Notify::new()),
            first_query_resume: Arc::new(tokio::sync::Notify::new()),
            pause_once: Mutex::new(true),
        }
    }
}

impl EmbeddingProvider for PausedEmbedding {
    fn space(&self) -> &EmbeddingSpace {
        &self.space
    }
    fn embed(&self, purpose: EmbeddingPurpose, _text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(async move {
            let pause = purpose == EmbeddingPurpose::Query && {
                let mut once = self.pause_once.lock().unwrap();
                std::mem::take(&mut *once)
            };
            if pause {
                self.first_query_started.notify_one();
                self.first_query_resume.notified().await;
            }
            Embedding::new(vec![1.0, 0.0], &self.space)
        })
    }
}

#[tokio::test]
async fn local_semantic_history_returns_backend_unavailable_without_keyword_fallback() {
    let _single_application = API_RUNTIME_TEST.lock().await;
    let app = ApplicationLifecycle::start(application_config(KnowledgeRetrieval::lexical()))
        .await
        .unwrap();
    ChatApi::publish_message(CharacterId::Crazy, UserInput::new("tea".into()).unwrap()).unwrap();
    assert!(matches!(
        HistorySearchApi::inspect_semantic(CharacterId::Crazy, "tea", FusionMode::Union, None)
            .await,
        Err(HistorySearchApiError::Search(
            HistorySearchError::BackendUnavailable
        ))
    ));
    assert_eq!(
        HistorySearchApi::inspect(CharacterId::Crazy, "tea")
            .await
            .unwrap()
            .hits
            .len(),
        1
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn live_semantic_history_revalidates_timeline_after_await() {
    let _single_application = API_RUNTIME_TEST.lock().await;
    let fixture = Arc::new(PausedEmbedding::new());
    let retriever = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 16).unwrap());
    let app =
        ApplicationLifecycle::start(application_config(KnowledgeRetrieval::semantic(retriever)))
            .await
            .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("tea in kitchen".into()).unwrap(),
    )
    .unwrap();
    let search = tokio::spawn(async {
        HistorySearchApi::inspect_semantic(CharacterId::Crazy, "tea", FusionMode::VectorOnly, None)
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.first_query_started.notified(),
    )
    .await
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("new private text".into()).unwrap(),
    )
    .unwrap();
    fixture.first_query_resume.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(5), search)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome,
        Err(HistorySearchApiError::StaleSelection)
    ));
    let refreshed =
        HistorySearchApi::inspect_semantic(CharacterId::Crazy, "tea", FusionMode::VectorOnly, None)
            .await
            .unwrap();
    assert_eq!(refreshed.hits.len(), 2);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn user_confirmed_cache_workflow_is_live_stale_safe_and_noncanonical() {
    let _single_application = API_RUNTIME_TEST.lock().await;
    let provider = Arc::new(PausedEmbedding::new());
    let retriever = Arc::new(SemanticMemoryRetriever::new(provider.clone(), 16).unwrap());
    let app = ApplicationLifecycle::start(application_config(KnowledgeRetrieval::semantic(
        retriever.clone(),
    )))
    .await
    .unwrap();
    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("tea on the balcony".into()).unwrap(),
    )
    .unwrap();
    let hits_before = HistorySearchApi::inspect(CharacterId::Crazy, "balcony")
        .await
        .unwrap();
    assert_eq!(hits_before.hits[0].key.id, event.0.to_string());
    let empty_preview = KnowledgeCacheApi::preview().unwrap();
    assert_eq!(empty_preview.total_entries, 0);

    let search = tokio::spawn(async {
        HistorySearchApi::inspect_semantic(CharacterId::Crazy, "tea", FusionMode::VectorOnly, None)
            .await
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        provider.first_query_started.notified(),
    )
    .await
    .unwrap();
    provider.first_query_resume.notify_one();
    assert_eq!(search.await.unwrap().unwrap().hits.len(), 1);
    let preview = KnowledgeCacheApi::preview().unwrap();
    assert_eq!(preview.total_entries, 1);
    assert_eq!(preview.entries_by_character[&CharacterId::Crazy], 1);
    assert!(
        !preview
            .entries_by_character
            .contains_key(&CharacterId::Kind)
    );

    assert!(matches!(
        KnowledgeCacheApi::clear_confirmed(&preview, false),
        Err(KnowledgeCacheApiError::Clear(
            neuromita::knowledge::KnowledgeCacheClearError::Maintenance(
                EmbeddingCacheMaintenanceError::ConfirmationRequired
            )
        ))
    ));
    assert_eq!(KnowledgeCacheApi::preview().unwrap().total_entries, 1);
    assert_eq!(
        KnowledgeCacheApi::clear_confirmed(&preview, true).unwrap(),
        1
    );
    assert_eq!(KnowledgeCacheApi::preview().unwrap().total_entries, 0);
    assert!(matches!(
        KnowledgeCacheApi::clear_confirmed(&preview, true),
        Err(KnowledgeCacheApiError::Clear(
            neuromita::knowledge::KnowledgeCacheClearError::Maintenance(
                EmbeddingCacheMaintenanceError::StalePreview
            )
        ))
    ));

    let after = HistorySearchApi::inspect(CharacterId::Crazy, "balcony")
        .await
        .unwrap();
    assert_eq!(after.hits[0].key.id, event.0.to_string());
    assert_eq!(after.hits[0].content, hits_before.hits[0].content);
    app.shutdown().await.unwrap();
    assert!(matches!(
        KnowledgeCacheApi::preview(),
        Err(KnowledgeCacheApiError::Unavailable)
    ));
}

#[tokio::test]
async fn lexical_mode_refuses_cache_preview_and_clear_without_silent_upgrade() {
    let _single_application = API_RUNTIME_TEST.lock().await;
    let app = ApplicationLifecycle::start(application_config(KnowledgeRetrieval::lexical()))
        .await
        .unwrap();
    assert!(matches!(
        KnowledgeCacheApi::preview(),
        Err(KnowledgeCacheApiError::BackendUnavailable)
    ));
    app.shutdown().await.unwrap();
}
