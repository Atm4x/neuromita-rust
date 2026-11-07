use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{HistoricalRecallApi, HistoricalRecallApiError},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::forgotten_recall::RecallSelector,
    memory::{MemoryCommand, MemoryPriority},
};

struct DisabledProvider;
impl GenerationProvider for DisabledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, _: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async {
            Err(GenerationError::Transport(
                "not used by historical recall".into(),
            ))
        })
    }
}

fn limits() -> RuntimeLimits {
    let positive = |value| NonZeroUsize::new(value).unwrap();
    RuntimeLimits {
        request_queue: positive(4),
        discovery_buffer: positive(8),
        character_commands: positive(16),
        presenter_updates: positive(8),
        retained_finished: positive(8),
    }
}

#[tokio::test]
async fn explicit_public_api_inspects_only_forgotten_facts_and_stops_with_runtime() {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture",
            "unused-key",
            GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([Arc::new(DisabledProvider) as Arc<dyn GenerationProvider>])
            .unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let owner = app.character(CharacterId::Crazy).unwrap();
    let other = app.character(CharacterId::Kind).unwrap();
    let first = owner.memories().await.unwrap();
    let next = owner
        .edit_memories(
            first,
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "forgotten orange tea".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "active purple tea".into(),
                },
            ],
        )
        .await
        .unwrap();
    let selected = next
        .snapshot
        .records()
        .iter()
        .find(|record| record.content == "forgotten orange tea")
        .unwrap()
        .id;
    owner
        .edit_memories(next.snapshot, vec![MemoryCommand::Forget { id: selected }])
        .await
        .unwrap();

    let text = HistoricalRecallApi::inspect(
        CharacterId::Crazy,
        RecallSelector::Ids(vec![selected]),
        "tea",
    )
    .await
    .unwrap()
    .unwrap();
    assert!(text.contains("[FORGOTTEN]"));
    assert!(text.contains("forgotten orange tea"));
    assert!(!text.contains("active purple tea"));
    assert!(
        HistoricalRecallApi::inspect(
            CharacterId::Kind,
            RecallSelector::Ids(vec![selected]),
            "tea",
        )
        .await
        .is_err()
    );
    assert_eq!(other.memories().await.unwrap().records().len(), 0);
    assert!(
        owner
            .memories()
            .await
            .unwrap()
            .records()
            .iter()
            .any(|record| record.id == selected
                && record.state == neuromita::memory::MemoryState::Forgotten)
    );

    app.shutdown().await.unwrap();
    assert!(matches!(
        HistoricalRecallApi::inspect(
            CharacterId::Crazy,
            RecallSelector::Ids(vec![selected]),
            "tea",
        )
        .await,
        Err(HistoricalRecallApiError::Unavailable)
    ));
}
