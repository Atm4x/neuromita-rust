use std::{num::NonZeroUsize, time::Duration};

use neuromita::{
    api::CharacterApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterError, CharacterId},
    generation::{
        GenerationPreset, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    memory::{MemoryCommand, MemoryPriority, MemoryState},
};

#[tokio::test]
async fn presenter_uses_the_same_character_owner_and_timeline_transaction() {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(
            "http://127.0.0.1:1/v1beta/",
            "test-model",
            "unused-key",
            GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let limits = RuntimeLimits {
        request_queue: NonZeroUsize::new(4).unwrap(),
        discovery_buffer: NonZeroUsize::new(16).unwrap(),
        character_commands: NonZeroUsize::new(16).unwrap(),
        presenter_updates: NonZeroUsize::new(16).unwrap(),
        retained_finished: NonZeroUsize::new(4).unwrap(),
    };
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new(Vec::new()).unwrap(),
        limits,
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let presenter = &app.presenter().characters;
    let snapshot = presenter.memories(CharacterId::Mita).await.unwrap();
    assert!(snapshot.records().is_empty());
    let after = presenter
        .edit_memories(
            CharacterId::Mita,
            snapshot.clone(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "A user-managed fact".to_owned(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(after.applied.len(), 1);
    assert_eq!(after.snapshot.records().len(), 1);
    assert_eq!(after.snapshot.records()[0].content, "A user-managed fact");
    assert_eq!(
        CharacterApi::get(CharacterId::Mita)
            .unwrap()
            .memories()
            .await
            .unwrap()
            .records(),
        after.snapshot.records()
    );
    assert!(matches!(
        presenter
            .edit_memories(
                CharacterId::Mita,
                snapshot,
                vec![MemoryCommand::Forget {
                    id: after.snapshot.records()[0].id
                }],
            )
            .await,
        Err(CharacterError::StaleState)
    ));
    let forgotten = presenter
        .edit_memories(
            CharacterId::Mita,
            after.snapshot,
            vec![MemoryCommand::Forget {
                id: after
                    .applied
                    .iter()
                    .find_map(|change| match change {
                        neuromita::memory::MemoryChange::Upsert(record) => Some(record.id),
                        _ => None,
                    })
                    .unwrap(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        forgotten.snapshot.records()[0].state,
        MemoryState::Forgotten
    );
    assert!(
        presenter
            .memories(CharacterId::KindMita)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}
