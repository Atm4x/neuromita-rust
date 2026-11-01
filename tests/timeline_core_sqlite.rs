#![cfg(feature = "sqlite-system")]

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    request::UserInput,
    timeline::{
        EventContent, TimelineConfig,
        domain::{BranchId, SessionId},
    },
};
use uuid::Uuid;

fn config(database: std::path::PathBuf, session: SessionId, root: BranchId) -> ApplicationConfig {
    let template = GeminiTemplate::google(
        "unused-model",
        "unused-test-key",
        GenerationSettings::new(0.0, 64, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(template));
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([Arc::new(GeminiProvider::new().unwrap())
            as Arc<dyn neuromita::generation::GenerationProvider>])
        .unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(8).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(8).unwrap(),
            retained_finished: NonZeroUsize::new(8).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.timeline = TimelineConfig::Sqlite {
        database,
        session,
        root,
    };
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_core_uses_durable_canonical_timeline_after_shutdown_restart() {
    let path = std::env::temp_dir().join(format!("neuromita-core-timeline-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let branch = BranchId::new();
    let first = ApplicationLifecycle::start(config(path.clone(), session, branch))
        .await
        .unwrap();
    let character = first.character(CharacterId::Mita).unwrap();
    let event = character
        .observe(UserInput::new("door opened".into()).unwrap())
        .await
        .unwrap();
    assert_eq!(first.at().through, 1);
    assert_eq!(first.timeline().len(), 1);
    first.shutdown().await.unwrap();

    let second = ApplicationLifecycle::start(config(path.clone(), session, branch))
        .await
        .unwrap();
    assert_eq!(second.at().through, 1);
    assert_eq!(second.at_event(event).unwrap().through, 1);
    assert!(matches!(
        second.timeline()[0].content,
        EventContent::Observation { .. }
    ));
    assert!(
        second
            .character(CharacterId::KindMita)
            .unwrap()
            .get_context(second.at())
            .await
            .unwrap()
            .history
            .events
            .is_empty()
    );
    second.shutdown().await.unwrap();
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}
