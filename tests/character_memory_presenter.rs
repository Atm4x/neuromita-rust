use std::{num::NonZeroUsize, time::Duration};

use neuromita::{
    api::CharacterApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterError, CharacterId},
    generation::{
        GenerationPreset, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    memory::{MemoryCommand, MemoryIsland, MemoryKind, MemoryPriority, MemoryState},
    presentation::MemoryListQuery,
};

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(4).unwrap(),
        discovery_buffer: NonZeroUsize::new(16).unwrap(),
        character_commands: NonZeroUsize::new(16).unwrap(),
        presenter_updates: NonZeroUsize::new(16).unwrap(),
        retained_finished: NonZeroUsize::new(4).unwrap(),
    }
}

async fn start() -> ApplicationLifecycle {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(
            "http://127.0.0.1:1/v1beta/",
            "test-model",
            "unused-key",
            GenerationSettings::new(0.7, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new(Vec::new()).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap()
}

#[tokio::test]
async fn presenter_memory_projection_filters_pages_and_keeps_edit_token() {
    let app = start().await;
    let presenter = &app.presenter().characters;
    let before = presenter.memories(CharacterId::Mita).await.unwrap();
    let after = presenter
        .edit_memories(
            CharacterId::Mita,
            before.clone(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "Мита любит чай".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Мита любит кофе".into(),
                },
                MemoryCommand::SetIsland {
                    island: MemoryIsland::Preferences,
                    content: "Предпочитает тихую музыку".into(),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(after.snapshot.records().len(), 3);
    assert_eq!(after.applied.len(), 3);
    assert!(matches!(
        presenter
            .edit_memories(
                CharacterId::Mita,
                before,
                vec![MemoryCommand::Forget {
                    id: after.snapshot.records()[0].id
                }]
            )
            .await,
        Err(CharacterError::StaleState)
    ));

    let page = presenter
        .list_memories(
            CharacterId::Mita,
            MemoryListQuery {
                text: Some("МИТА".into()),
                kind: Some(MemoryKind::Fact),
                limit: 1,
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.total_matches(), 2);
    assert_eq!(page.records().len(), 1);
    assert_eq!(page.next_offset(), Some(1));
    assert_eq!(page.summary().active, 3);
    assert_eq!(page.summary().facts, 2);
    assert_eq!(page.summary().islands, 1);
    assert_eq!(page.snapshot().records().len(), 3);

    let second = presenter
        .list_memories(
            CharacterId::Mita,
            MemoryListQuery {
                text: Some("мита".into()),
                kind: Some(MemoryKind::Fact),
                offset: page.next_offset().unwrap(),
                limit: 1,
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.records().len(), 1);
    assert_ne!(page.records()[0].id, second.records()[0].id);
    assert_eq!(second.next_offset(), None);

    let target = page.records()[0].id;
    let retained_page = page.clone();
    let edited = presenter
        .edit_memories(
            CharacterId::Mita,
            page.into_snapshot(),
            vec![MemoryCommand::Forget { id: target }],
        )
        .await
        .unwrap();
    assert!(
        edited
            .snapshot
            .records()
            .iter()
            .any(|r| r.id == target && r.state == MemoryState::Forgotten)
    );
    let pinned = neuromita::presentation::MemoryListPage::from_snapshot(
        retained_page.snapshot().clone(),
        MemoryListQuery {
            state: Some(MemoryState::Forgotten),
            ..MemoryListQuery::default()
        },
    )
    .unwrap();
    assert_eq!(pinned.total_matches(), 0);
    assert_eq!(pinned.summary().active, 3);
    assert_eq!(
        CharacterApi::get(CharacterId::Mita)
            .unwrap()
            .memories()
            .await
            .unwrap()
            .records(),
        edited.snapshot.records()
    );
    let forgotten = presenter
        .list_memories(
            CharacterId::Mita,
            MemoryListQuery {
                state: Some(MemoryState::Forgotten),
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(forgotten.total_matches(), 1);
    assert_eq!(forgotten.summary().forgotten, 1);
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

#[tokio::test]
async fn memory_presenter_rejects_invalid_queries_without_mutating_character() {
    let app = start().await;
    let presenter = &app.presenter().characters;
    for query in [
        MemoryListQuery {
            limit: 0,
            ..MemoryListQuery::default()
        },
        MemoryListQuery {
            limit: 101,
            ..MemoryListQuery::default()
        },
        MemoryListQuery {
            text: Some("x".repeat(201)),
            ..MemoryListQuery::default()
        },
    ] {
        assert!(matches!(
            presenter.list_memories(CharacterId::Mita, query).await,
            Err(CharacterError::InvalidMemory(_))
        ));
    }
    let empty = presenter
        .list_memories(
            CharacterId::Mita,
            MemoryListQuery {
                offset: usize::MAX,
                ..MemoryListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(empty.records().is_empty());
    assert_eq!(empty.next_offset(), None);
    assert_eq!(empty.total_matches(), 0);
    assert!(
        presenter
            .memories(CharacterId::Mita)
            .await
            .unwrap()
            .records()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}
