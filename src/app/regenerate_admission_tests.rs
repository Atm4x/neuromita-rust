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

#[tokio::test]
async fn regenerate_receipt_is_exact_queued_request_and_survives_cancel() {
    let mut fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let child = BranchId::new();
    let handle = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    let owner = fixture.timeline.lock().unwrap();
    let receipt = owner.regenerate_admission_receipt(child).unwrap().unwrap();
    assert_eq!(receipt.request, handle.id());
    assert_eq!(receipt.accepted_request, fixture.source.accepted_request);
    assert_eq!(receipt.accepted_message, fixture.source.accepted_message);
    assert_eq!(receipt.original_input, fixture.source.original_input);
    assert_eq!(receipt.parent_head, fixture.fence.at.through);
    assert_eq!(
        receipt.parent_generation,
        fixture.fence.selection.generation
    );
    assert_eq!(receipt.child_branch, child);
    assert_eq!(receipt.configuration.provider_model, "model");
    drop(owner);
    assert!(matches!(
        handle.cancel(),
        crate::request::CancelResult::Cancelled
    ));
    assert_eq!(fixture.queue.try_recv().unwrap(), handle.id());
    assert_eq!(
        fixture
            .timeline
            .lock()
            .unwrap()
            .regenerate_admission_receipt(child)
            .unwrap()
            .unwrap(),
        receipt
    );
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn sqlite_receipt_commits_before_volatile_request_registration_and_reopens() {
    let db = std::env::temp_dir().join(format!("b62-durable-receipt-{}.db", uuid::Uuid::new_v4()));
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
    let id = RequestId::new();
    let receipt = crate::timeline::RegenerateAdmissionReceipt {
        request: id,
        accepted_request: fixture.source.accepted_request,
        accepted_message: fixture.source.accepted_message,
        original_input: fixture.source.original_input,
        character: fixture.source.character,
        parent_branch: fixture.fence.selection.branch_id,
        parent_head: fixture.fence.at.through,
        parent_generation: fixture.fence.selection.generation,
        child_branch: child,
        configuration: crate::timeline::RegenerateConfigurationReference {
            settings_revision: 1,
            preset_id: 0,
            provider_model: "model".into(),
            prompt_set: None,
        },
    };
    fixture
        .timeline
        .lock()
        .unwrap()
        .fork_and_admit_regenerate_source(
            fixture.source,
            child,
            fixture.fence.selection,
            fixture.fence.at.through,
            &receipt,
        )
        .unwrap();
    assert!(fixture.api.registry.get(id).is_none());
    assert_eq!(
        fixture
            .timeline
            .lock()
            .unwrap()
            .regenerate_admission_receipt(child)
            .unwrap(),
        Some(receipt.clone())
    );
    drop(fixture);
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.selection().branch_id, child);
    assert_eq!(
        reopened.regenerate_admission_receipt(child).unwrap(),
        Some(receipt.clone())
    );
    let repository = crate::timeline::storage::SqliteTimelineRepository::open(&db).unwrap();
    let persisted = repository.archive_snapshot().unwrap();
    let mut branches = persisted.persisted_branches();
    let admitted = branches
        .iter_mut()
        .find(|branch| branch.info.id == child)
        .unwrap();
    let marker = &mut admitted.events[0].draft.payload;
    let data = marker
        .get_mut(crate::timeline::RegenerateAdmissionReceipt::KEY)
        .unwrap();
    data["accepted_message"] = json!(EventId::new());
    assert!(crate::timeline::domain::TimelineStore::load_persisted(branches).is_err());
    drop((repository, reopened));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn native_archive_round_trip_preserves_exact_regenerate_admission() {
    use crate::timeline::storage::{SqliteTimelineRepository, archive, assets::TimelineAssetStore};
    let db = std::env::temp_dir().join(format!("b62-archive-source-{}.db", uuid::Uuid::new_v4()));
    let dst = std::env::temp_dir().join(format!("b62-archive-target-{}.db", uuid::Uuid::new_v4()));
    let assets_dir =
        std::env::temp_dir().join(format!("b62-archive-assets-{}", uuid::Uuid::new_v4()));
    let assets = TimelineAssetStore::new(&assets_dir, 1024 * 1024).unwrap();
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
    let handle = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    let saved = fixture
        .timeline
        .lock()
        .unwrap()
        .regenerate_admission_receipt(child)
        .unwrap()
        .unwrap();
    assert_eq!(saved.request, handle.id());
    let repository = SqliteTimelineRepository::open(&db).unwrap();
    let bytes = archive::export_sqlite(&repository, session, &assets).unwrap();
    let mut restored = SqliteTimelineRepository::open(&dst).unwrap();
    assert_eq!(
        archive::import_sqlite(&mut restored, &bytes, &assets).unwrap(),
        session
    );
    let marker = restored
        .archive_snapshot()
        .unwrap()
        .world(child)
        .unwrap()
        .into_iter()
        .find(|fact| {
            fact.branch_id == child
                && fact.draft.kind == crate::timeline::domain::EventKind::BranchCreated
        })
        .unwrap();
    assert_eq!(
        crate::timeline::RegenerateAdmissionReceipt::from_marker(&marker.draft.payload).unwrap(),
        Some(saved.clone())
    );
    let mut tampered: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for branch in tampered["branches"].as_array_mut().unwrap() {
        if branch["info"]["id"] == json!(child) {
            branch["events"][0]["draft"]["payload"]["regenerate_admission"]["original_input"] =
                json!(EventId::new());
        }
    }
    let tampered = serde_json::to_vec(&tampered).unwrap();
    let other = std::env::temp_dir().join(format!("b62-tamper-target-{}.db", uuid::Uuid::new_v4()));
    let mut rejected = SqliteTimelineRepository::open(&other).unwrap();
    assert!(archive::import_sqlite(&mut rejected, &tampered, &assets).is_err());
    drop((fixture, repository, restored, rejected));
    for path in [&db, &dst, &other] {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }
    let _ = std::fs::remove_dir_all(assets_dir);
}

#[tokio::test]
async fn provider_dispatch_fence_persists_before_external_call_and_denies_duplicate() {
    let mut case = fixture(TimelineConfig::Transient, 2, true).await;
    let child = BranchId::new();
    let handle = case.api.regenerate(case.source, case.fence, child).unwrap();
    let id = case.queue.try_recv().unwrap();
    assert_eq!(id, handle.id());
    let admission = case.api.registry.get(id).unwrap().admission();
    let fence = TimelineRequestIngress {
        timeline: Arc::clone(&case.timeline),
    };
    fence.before_provider(id, admission).unwrap();
    let owner = case.timeline.lock().unwrap();
    assert!(matches!(
        owner.reconcile_regenerate(child, id).unwrap().outcome,
        crate::timeline::RegenerateRecoveryOutcome::MayHaveDispatched(_)
    ));
    drop(owner);
    assert!(fence.before_provider(id, admission).is_err());
}

#[tokio::test]
async fn normal_root_request_never_requires_a_regenerate_dispatch_marker() {
    let fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let owner = fixture.timeline.lock().unwrap();
    let initial = owner.anchor();
    let admission = owner
        .admission(CharacterId::Crazy, owner.selection())
        .unwrap();
    drop(owner);
    let fence = TimelineRequestIngress {
        timeline: Arc::clone(&fixture.timeline),
    };
    fence.before_provider(RequestId::new(), admission).unwrap();
    let owner = fixture.timeline.lock().unwrap();
    assert_eq!(owner.anchor(), initial);
}

#[tokio::test]
async fn unrelated_request_on_regenerate_child_does_not_consume_dispatch_permit() {
    let fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let child = BranchId::new();
    let admitted = fixture
        .api
        .regenerate(fixture.source, fixture.fence, child)
        .unwrap();
    let owner = fixture.timeline.lock().unwrap();
    let admission = owner
        .admission(CharacterId::Crazy, owner.selection())
        .unwrap();
    drop(owner);
    let fence = TimelineRequestIngress {
        timeline: Arc::clone(&fixture.timeline),
    };
    fence.before_provider(RequestId::new(), admission).unwrap();
    assert!(matches!(
        fixture
            .timeline
            .lock()
            .unwrap()
            .reconcile_regenerate(child, admitted.id())
            .unwrap()
            .outcome,
        crate::timeline::RegenerateRecoveryOutcome::DispatchUnknown
    ));
}
