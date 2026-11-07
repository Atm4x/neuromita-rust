use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    UserInput,
    api::{ChatApi, HistorySearchApi, HistorySearchApiError},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::history_retrieval::HistorySearchStatus,
};

struct DisabledProvider;
impl GenerationProvider for DisabledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, _: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async { Err(GenerationError::Transport("unused".into())) })
    }
}

#[tokio::test]
async fn local_history_api_reads_live_scoped_timeline_and_cannot_generate() {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture",
            "unused-key",
            GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let capacity = |size| NonZeroUsize::new(size).unwrap();
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
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
    ))
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
