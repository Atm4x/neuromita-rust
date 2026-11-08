use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::json;

use super::*;
use crate::{
    generation::{
        ProviderTemplate,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::TimelineFence,
    timeline::{
        conversation::ConversationSpeaker,
        domain::{CausalLink, CausalRelation, TimelineRepository},
    },
};

struct Fixture {
    api: Arc<ChatApiState>,
    queue: mpsc::Receiver<RequestId>,
    sender: mpsc::Sender<RequestId>,
    timeline: Arc<Mutex<SessionTimeline>>,
    source: RegenerateSource,
    fence: TimelineFence,
}

async fn fixture(config: TimelineConfig, queue_capacity: usize, with_preset: bool) -> Fixture {
    let timeline = Arc::new(Mutex::new(SessionTimeline::from_config(config).unwrap()));
    let (source, fence) = {
        let mut owner = timeline.lock().unwrap();
        let original_input = EventId::new();
        owner
            .publish_player_message(CharacterId::Crazy, original_input, Arc::from("source"))
            .unwrap();
        let accepted_request = RequestId::new();
        let mut reply = ConversationMessage::new(
            EventId::new().0.to_string(),
            ConversationSpeaker::Character(CharacterId::Crazy),
            json!("accepted"),
            [CharacterId::Crazy],
        );
        let accepted_message = reply.id;
        reply.request_id = Some(accepted_request);
        reply.reply_to = Some(original_input);
        reply.links.push(CausalLink {
            relation: CausalRelation::ReplyTo,
            event_id: original_input,
        });
        let selection = owner.selection();
        owner
            .publish_conversation_message(selection, reply)
            .unwrap();
        let source = RegenerateSource {
            character: CharacterId::Crazy,
            accepted_message,
            accepted_request,
            original_input,
        };
        let fence = TimelineFence {
            selection: owner.selection(),
            at: owner.anchor(),
        };
        (source, fence)
    };
    let settings = SettingsLifecycle::prepare(SettingsConfig {
        source: SettingsSource::initial(AppSettings::initial_profile()),
        repository: Arc::new(MemorySettingsRepository::new()),
    })
    .await
    .unwrap();
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(
            "http://127.0.0.1:56789/",
            "model",
            "test-key",
            GenerationSettings::new(0.5, 100, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let presets = if with_preset {
        PresetCatalog::single(preset)
    } else {
        PresetCatalog::new(Default::default())
    };
    let (sender, queue) = mpsc::channel(queue_capacity);
    let api = Arc::new(ChatApiState::new(
        Arc::new(RequestRegistry::new(8)),
        HashMap::from([(CharacterId::Crazy, sender.clone())]),
        Arc::new(TimelineRequestIngress {
            timeline: Arc::clone(&timeline),
        }),
        settings.owner(),
        Arc::new(presets),
        None,
    ));
    Fixture {
        api,
        queue,
        sender,
        timeline,
        source,
        fence,
    }
}

fn unchanged(fixture: &Fixture, child: BranchId) {
    let owner = fixture.timeline.lock().unwrap();
    assert_eq!(owner.selection(), fixture.fence.selection);
    assert_eq!(owner.anchor(), fixture.fence.at);
    assert_ne!(owner.selection().branch_id, child);
}

#[tokio::test]
async fn queue_full_and_closed_leave_original_world_unchanged() {
    let mut full = fixture(TimelineConfig::Transient, 1, true).await;
    let child = BranchId::new();
    full.sender.try_send(RequestId::new()).unwrap();
    assert!(matches!(
        full.api.regenerate(full.source, full.fence, child),
        Err(SubmitError::QueueFull)
    ));
    unchanged(&full, child);
    let _ = full.queue.recv().await;
    let closed = fixture(TimelineConfig::Transient, 1, true).await;
    let child = BranchId::new();
    let Fixture {
        api,
        queue,
        sender: _,
        timeline,
        source,
        fence,
    } = closed;
    drop(queue);
    assert!(matches!(
        api.regenerate(source, fence, child),
        Err(SubmitError::QueueClosed)
    ));
    let owner = timeline.lock().unwrap();
    assert_eq!(owner.selection(), fence.selection);
    assert_ne!(owner.selection().branch_id, child);
}

#[tokio::test]
async fn missing_preset_and_stale_head_do_not_create_child() {
    let without_preset = fixture(TimelineConfig::Transient, 2, false).await;
    let child = BranchId::new();
    assert!(matches!(
        without_preset
            .api
            .regenerate(without_preset.source, without_preset.fence, child),
        Err(SubmitError::PresetUnavailable)
    ));
    unchanged(&without_preset, child);

    let stale = fixture(TimelineConfig::Transient, 2, true).await;
    let next = EventId::new();
    stale
        .timeline
        .lock()
        .unwrap()
        .publish_player_message(CharacterId::Crazy, next, Arc::from("newer input"))
        .unwrap();
    let child = BranchId::new();
    assert!(matches!(
        stale.api.regenerate(stale.source, stale.fence, child),
        Err(SubmitError::StaleTimeline)
    ));
    let owner = stale.timeline.lock().unwrap();
    assert_ne!(owner.selection().branch_id, child);
    assert_eq!(owner.selection(), stale.fence.selection);
}

#[tokio::test]
async fn successful_request_is_queued_once_on_the_exact_child_admission() {
    let mut fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let child = BranchId::new();
    let handle = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    assert_eq!(fixture.queue.try_recv().unwrap(), handle.id());
    assert!(fixture.queue.try_recv().is_err());
    let record = fixture.api.registry.get(handle.id()).unwrap();
    let owner = fixture.timeline.lock().unwrap();
    assert_eq!(record.admission().selection, owner.selection());
    owner
        .assert_admission(CharacterId::Crazy, record.admission())
        .unwrap();
    assert_eq!(
        record.snapshot().metadata.intent,
        TurnIntent::Reply(fixture.source.original_input)
    );
    assert!(record.admitted_settings().is_some());
    assert!(record.admitted_preset().is_some());
    assert_eq!(
        owner
            .at_event(fixture.source.original_input)
            .unwrap()
            .branch_id,
        child.0
    );
    assert!(owner.at_event(fixture.source.accepted_message).is_none());
}

#[tokio::test]
async fn two_concurrent_regenerate_requests_have_exactly_one_winner() {
    let mut fixture = fixture(TimelineConfig::Transient, 3, true).await;
    let children = [BranchId::new(), BranchId::new()];
    let api = Arc::clone(&fixture.api);
    let source = fixture.source;
    let fence = fixture.fence;
    let outcomes = std::thread::scope(|scope| {
        let first = scope.spawn(|| api.regenerate(source, fence, children[0]));
        let second = scope.spawn(|| fixture.api.regenerate(source, fence, children[1]));
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(SubmitError::StaleTimeline)))
            .count(),
        1
    );
    let committed = fixture.timeline.lock().unwrap().selection().branch_id;
    assert!(children.contains(&committed));
    assert_eq!(
        fixture.queue.try_recv().unwrap(),
        outcomes.into_iter().find_map(Result::ok).unwrap().id()
    );
    assert!(fixture.queue.try_recv().is_err());
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn sqlite_reopen_sees_one_selected_child_with_unmodified_input() {
    let db = std::env::temp_dir().join(format!("b61-admission-{}.db", uuid::Uuid::new_v4()));
    let session = crate::timeline::domain::SessionId::new();
    let root = BranchId::new();
    let config = TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    };
    let mut fixture = fixture(config, 2, true).await;
    let child = BranchId::new();
    let handle = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    let selected = fixture.api.registry.get(handle.id()).unwrap().admission();
    let original = fixture.source.original_input;
    assert_eq!(fixture.queue.try_recv().unwrap(), handle.id());
    drop(fixture);
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.selection(), selected.selection);
    assert_eq!(reopened.at_event(original).unwrap().branch_id, child.0);
    reopened
        .assert_admission(CharacterId::Crazy, selected)
        .unwrap();
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[tokio::test]
async fn simultaneous_player_publish_and_regenerate_are_linearizable() {
    let mut fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let child = BranchId::new();
    let mut message = ConversationMessage::new(
        EventId::new().0.to_string(),
        ConversationSpeaker::Player,
        json!("new player turn"),
        [CharacterId::Crazy],
    );
    let input = message.id;
    message
        .audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Crazy));
    let gate = Arc::new(std::sync::Barrier::new(3));
    let api = Arc::clone(&fixture.api);
    let source = fixture.source;
    let fence = fixture.fence;
    let (admitted, published) = std::thread::scope(|scope| {
        let regen_gate = Arc::clone(&gate);
        let regen = scope.spawn(move || {
            regen_gate.wait();
            api.regenerate(source, fence, child)
        });
        let publish_gate = Arc::clone(&gate);
        let publish_api = Arc::clone(&fixture.api);
        let publish = scope.spawn(move || {
            publish_gate.wait();
            publish_api.publish_conversation_message(message)
        });
        gate.wait();
        (regen.join().unwrap(), publish.join().unwrap())
    });
    assert_eq!(published.unwrap(), input);
    let owner = fixture.timeline.lock().unwrap();
    assert!(owner.at_event(input).is_some());
    match admitted {
        Ok(handle) => {
            assert_eq!(owner.selection().branch_id, child);
            assert_eq!(fixture.queue.try_recv().unwrap(), handle.id());
        }
        Err(SubmitError::StaleTimeline) => {
            assert_eq!(owner.selection(), fence.selection);
            assert!(fixture.queue.try_recv().is_err());
        }
        other => panic!("unexpected admission outcome: {other:?}"),
    }
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn sqlite_selection_failure_aborts_regenerate_before_queue_registration() {
    let db = std::env::temp_dir().join(format!("b61-sql-abort-{}.db", uuid::Uuid::new_v4()));
    let session = crate::timeline::domain::SessionId::new();
    let root = BranchId::new();
    let fixture = fixture(
        TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root,
        },
        2,
        true,
    )
    .await;
    let child = BranchId::new();
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection.execute_batch("CREATE TRIGGER b61_fail BEFORE UPDATE ON timeline_active_selection BEGIN SELECT RAISE(ABORT, 'injected selection failure'); END;").unwrap();
    assert!(matches!(
        fixture.api.regenerate(fixture.source, fixture.fence, child),
        Err(SubmitError::TimelineUnavailable)
    ));
    unchanged(&fixture, child);
    let repository = crate::timeline::storage::SqliteTimelineRepository::open(&db).unwrap();
    assert!(matches!(
        repository.branch(child),
        Err(TimelineError::NotFound)
    ));
    connection.execute_batch("DROP TRIGGER b61_fail;").unwrap();
    let admitted = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    assert_eq!(
        admitted.id(),
        fixture
            .api
            .registry
            .get(admitted.id())
            .unwrap()
            .snapshot()
            .metadata
            .id
    );
    assert_eq!(
        fixture.timeline.lock().unwrap().selection().branch_id,
        child
    );
    drop(fixture);
    drop(repository);
    drop(connection);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[tokio::test]
async fn cancelling_after_admission_does_not_erase_committed_history() {
    let mut fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let child = BranchId::new();
    let handle = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    let admitted = fixture.api.registry.get(handle.id()).unwrap().admission();
    assert!(matches!(
        handle.cancel(),
        crate::request::CancelResult::Cancelled
    ));
    assert_eq!(fixture.queue.try_recv().unwrap(), handle.id());
    let owner = fixture.timeline.lock().unwrap();
    assert_eq!(owner.selection().branch_id, child);
    owner
        .assert_admission(CharacterId::Crazy, admitted)
        .unwrap();
    assert_eq!(
        owner
            .at_event(fixture.source.original_input)
            .unwrap()
            .branch_id,
        child.0
    );
}
