#![cfg(feature = "sqlite-system")]

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    request::{ReplyRequest, RequestSource, UserInput},
    timeline::{
        EventContent, TimelineConfig,
        domain::{BranchId, SessionId},
    },
};
use uuid::Uuid;

static CORE_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _serial = CORE_TEST_MUTEX.lock().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn core_restores_durable_active_branch_and_character_state() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    use neuromita::timeline::{
        domain::{EventDraft, EventKind, TimelineRepository},
        storage::SqliteTimelineRepository,
    };
    use serde_json::json;
    let path = std::env::temp_dir().join(format!("neuromita-branch-state-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let child = BranchId::new();
    let mut repo = SqliteTimelineRepository::open(&path).unwrap();
    repo.create_session(session, root).unwrap();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint
        .payload
        .insert("snapshot".into(), json!({"state": "before"}));
    let checkpoint_id = checkpoint.id;
    repo.append(root, 0, "checkpoint", json!({}), vec![checkpoint])
        .unwrap();
    repo.fork(root, checkpoint_id, child).unwrap();
    drop(repo);

    let app = ApplicationLifecycle::start(config(path.clone(), session, root))
        .await
        .unwrap();
    let old_selection = app.timeline_selection();
    assert_eq!(old_selection.branch_id, root);
    let selected = app.select_timeline_branch(child, old_selection).unwrap();
    assert_eq!(app.at().branch_id, child.0);
    assert_eq!(app.timeline_selection(), selected);
    assert!(app.select_timeline_branch(root, old_selection).is_err());
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    app.shutdown().await.unwrap();

    let reopened = ApplicationLifecycle::start(config(path.clone(), session, root))
        .await
        .unwrap();
    assert_eq!(reopened.timeline_selection(), selected);
    assert_eq!(reopened.at().branch_id, child.0);
    assert_eq!(reopened.at().through, 2);
    reopened.shutdown().await.unwrap();
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_player_message_survives_cancel_and_sqlite_restart() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let path = std::env::temp_dir().join(format!("neuromita-ingress-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let branch = BranchId::new();
    let app = ApplicationLifecycle::start(config(path.clone(), session, branch))
        .await
        .unwrap();
    let character = CharacterId::Mita;
    let template = GeminiTemplate::google(
        "unused-model",
        "unused-test-key",
        GenerationSettings::new(0.0, 64, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    let handle = ChatApi::reply(ReplyRequest::new(
        character,
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        UserInput::new("persisted before generation".into()).unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    assert!(app.timeline().iter().any(|event| {
        event.request_id == Some(handle.id())
            && matches!(event.content, EventContent::PlayerMessage { .. })
    }));
    handle.cancel();
    app.shutdown().await.unwrap();
    let reopened = ApplicationLifecycle::start(config(path.clone(), session, branch))
        .await
        .unwrap();
    assert!(reopened.timeline().iter().any(|event| {
        event.request_id == Some(handle.id())
            && matches!(event.content, EventContent::PlayerMessage { .. })
    }));
    assert!(
        reopened
            .character(character)
            .unwrap()
            .get_context(reopened.at())
            .await
            .unwrap()
            .view
            .messages()
            .iter()
            .any(|message| message.content == "persisted before generation")
    );
    reopened.shutdown().await.unwrap();
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
