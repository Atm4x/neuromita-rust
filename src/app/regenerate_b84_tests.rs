use super::*;

fn orphan_settings() -> RegenerateConfigurationReference {
    RegenerateConfigurationReference {
        settings_revision: 1,
        preset_id: 0,
        provider_model: "b84-controlled".to_owned(),
        prompt_set: None,
    }
}

fn restart_api(
    db: &std::path::Path,
    session: crate::timeline::domain::SessionId,
    root: BranchId,
    settings: Arc<crate::settings::SettingsOwner>,
    presets: Arc<PresetCatalog>,
) -> (
    ChatApiState,
    Arc<Mutex<SessionTimeline>>,
    mpsc::Receiver<RequestId>,
) {
    let timeline = Arc::new(Mutex::new(
        SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.to_owned(),
            session,
            root,
        })
        .unwrap(),
    ));
    let (sender, queue) = mpsc::channel(2);
    let api = ChatApiState::new(
        Arc::new(RequestRegistry::new(8)),
        HashMap::from([(CharacterId::Crazy, sender)]),
        Arc::new(TimelineRequestIngress {
            timeline: Arc::clone(&timeline),
        }),
        settings,
        presets,
        None,
    );
    (api, timeline, queue)
}

#[tokio::test]
async fn b84_crash_gap_orphan_requires_explicit_abandonment_and_survives_archive() {
    use crate::timeline::storage::{SqliteTimelineRepository, archive, assets::TimelineAssetStore};

    let db = std::env::temp_dir().join(format!("b84-crash-gap-{}.db", uuid::Uuid::new_v4()));
    let assets_root = std::env::temp_dir().join(format!("b84-assets-{}", uuid::Uuid::new_v4()));
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
    let source = fixture.source;
    let other_mita = EventId::new();
    let fence = {
        let mut owner = fixture.timeline.lock().unwrap();
        let mut message = ConversationMessage::new(
            other_mita.0.to_string(),
            ConversationSpeaker::Character(CharacterId::Kind),
            json!("unrelated Kind reply"),
            [CharacterId::Kind],
        );
        message.id = other_mita;
        let selection = owner.selection();
        owner
            .publish_conversation_message(selection, message)
            .unwrap();
        TimelineFence {
            selection: owner.selection(),
            at: owner.anchor(),
        }
    };
    let request = RequestId::new();
    let ingress = TimelineRequestIngress {
        timeline: Arc::clone(&fixture.timeline),
    };
    let admission = ingress
        .admit_same_branch_regenerate(source, fence, request, orphan_settings())
        .unwrap();
    assert_eq!(admission.selection.branch_id, root);
    assert!(fixture.api.registry.get(request).is_none());
    assert!(fixture.queue.is_empty());
    assert_eq!(
        fixture
            .api
            .inspect_orphaned_same_branch_regenerate(root, request)
            .unwrap(),
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    let settings = Arc::clone(&fixture.settings_owner);
    let presets = Arc::clone(&fixture.presets);
    drop((ingress, fixture));

    let (api, reopened, mut queue) = restart_api(&db, session, root, settings, presets);
    api.close();
    assert_eq!(
        api.inspect_orphaned_same_branch_regenerate(root, request)
            .unwrap(),
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    assert!(queue.try_recv().is_err());
    assert!(api.registry.get(request).is_none());
    let before = reopened.lock().unwrap().anchor();
    assert_eq!(
        api.abandon_orphaned_same_branch_regenerate(root, RequestId::new()),
        Err(TimelineError::NotFound)
    );
    assert_eq!(reopened.lock().unwrap().anchor(), before);
    let first = api
        .abandon_orphaned_same_branch_regenerate(root, request)
        .unwrap();
    assert!(matches!(first, RegenerateRecoveryOutcome::Abandoned(_)));
    assert_eq!(
        api.abandon_orphaned_same_branch_regenerate(root, request)
            .unwrap(),
        first
    );
    assert!(queue.try_recv().is_err());
    assert!(api.registry.get(request).is_none());
    assert!(ingress_dispatch_denied(&reopened, request, admission));
    let owner = reopened.lock().unwrap();
    assert_eq!(
        owner
            .knows_event_at(CharacterId::Crazy, source.accepted_message, None)
            .unwrap(),
        false
    );
    assert!(
        owner
            .knows_event_at(
                CharacterId::Crazy,
                source.accepted_message,
                Some(fence.at.through)
            )
            .unwrap()
    );
    drop(owner);
    let snapshot = SqliteTimelineRepository::open(&db)
        .unwrap()
        .archive_snapshot()
        .unwrap();
    let world = snapshot.world(root).unwrap();
    assert!(
        world
            .iter()
            .any(|fact| fact.draft.id == source.original_input)
    );
    assert!(world.iter().any(|fact| fact.draft.id == other_mita));

    let assets = TimelineAssetStore::new(&assets_root, 1024 * 1024).unwrap();
    let repository = SqliteTimelineRepository::open(&db).unwrap();
    let bytes = archive::export_sqlite(&repository, session, &assets).unwrap();
    let mut restored = crate::timeline::domain::TimelineStore::new();
    archive::import_memory(&mut restored, &bytes, &assets).unwrap();
    let facts = restored.world(root).unwrap();
    assert_eq!(
        facts
            .iter()
            .filter(
                |fact| fact.draft.kind == crate::timeline::domain::EventKind::RegenerateAbandoned
            )
            .count(),
        1
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.draft.kind
                == crate::timeline::domain::EventKind::RegenerateMayHaveDispatched)
            .count(),
        0
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.draft.id == other_mita)
            .count(),
        1
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.draft.id == source.original_input)
            .count(),
        1
    );
    drop((api, reopened, repository));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
    let _ = std::fs::remove_dir_all(assets_root);
}

fn ingress_dispatch_denied(
    timeline: &Arc<Mutex<SessionTimeline>>,
    request: RequestId,
    admission: crate::timeline::selection::TimelineAdmission,
) -> bool {
    TimelineRequestIngress {
        timeline: Arc::clone(timeline),
    }
    .before_provider(request, admission, Some(RegenerateProtocol::SameBranch))
    .is_err()
}

#[tokio::test]
async fn b84_orphan_abandonment_refuses_live_request_registry_owner() {
    let fixture = fixture(TimelineConfig::Transient, 2, true).await;
    let handle = fixture
        .api
        .regenerate_same_branch(fixture.source, fixture.fence)
        .unwrap();
    assert_eq!(
        fixture.api.abandon_orphaned_same_branch_regenerate(
            fixture.fence.selection.branch_id,
            handle.id()
        ),
        Err(TimelineError::Conflict)
    );
    assert_eq!(
        fixture
            .api
            .inspect_orphaned_same_branch_regenerate(fixture.fence.selection.branch_id, handle.id())
            .unwrap(),
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    assert!(matches!(
        handle.cancel(),
        crate::request::CancelResult::Cancelled | crate::request::CancelResult::Requested
    ));
}

#[tokio::test]
async fn b84_orphaned_external_acceptance_cannot_be_relabelled_abandoned() {
    let db = std::env::temp_dir().join(format!("b84-accepted-{}.db", uuid::Uuid::new_v4()));
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
    let request = RequestId::new();
    let ingress = TimelineRequestIngress {
        timeline: Arc::clone(&fixture.timeline),
    };
    let admission = ingress
        .admit_same_branch_regenerate(fixture.source, fixture.fence, request, orphan_settings())
        .unwrap();
    ingress
        .before_provider(request, admission, Some(RegenerateProtocol::SameBranch))
        .unwrap();
    let source = fixture.source;
    let settings = Arc::clone(&fixture.settings_owner);
    let presets = Arc::clone(&fixture.presets);
    drop((ingress, fixture));
    let mut foreign = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let turn = foreign
        .capture_turn(
            source.character,
            Some(source.original_input),
            foreign.anchor(),
        )
        .unwrap();
    let revision = foreign
        .accepted_replies_at(source.character, foreign.anchor())
        .unwrap();
    let accepted = foreign
        .append_reply(crate::timeline::GeneratedReply {
            request,
            intent: TurnIntent::Reply(source.original_input),
            turn,
            expected_revision: revision,
            content: json!("orphan accepted by foreign SQLite owner"),
            audience: crate::timeline::conversation::ConversationAudience::private_to(
                source.character,
            ),
            effects: vec![
                crate::timeline::domain::AppliedEffect::CharacterStateChanged(
                    crate::timeline::domain::CharacterBehavior {
                        attitude: 82.0,
                        boredom: 1.0,
                        stress: 3.0,
                    },
                ),
            ],
        })
        .unwrap();
    drop(foreign);
    let (api, reopened, mut queue) = restart_api(&db, session, root, settings, presets);
    assert_eq!(
        api.abandon_orphaned_same_branch_regenerate(root, request)
            .unwrap(),
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    assert_eq!(
        api.abandon_orphaned_same_branch_regenerate(root, request)
            .unwrap(),
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    assert!(queue.try_recv().is_err());
    assert!(api.registry.get(request).is_none());
    let owner = reopened.lock().unwrap();
    assert_eq!(
        owner
            .character_behavior_at(CharacterId::Crazy, owner.anchor())
            .unwrap()
            .attitude,
        82.0
    );
    drop(owner);
    let snapshot = crate::timeline::storage::SqliteTimelineRepository::open(&db)
        .unwrap()
        .archive_snapshot()
        .unwrap();
    assert_eq!(
        snapshot
            .world(root)
            .unwrap()
            .iter()
            .filter(
                |fact| fact.draft.kind == crate::timeline::domain::EventKind::RegenerateAbandoned
            )
            .count(),
        0
    );
    drop((reopened, api));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[tokio::test]
async fn b84_two_sqlite_owners_abandon_same_orphan_without_double_terminal() {
    let db = std::env::temp_dir().join(format!("b84-dual-owner-{}.db", uuid::Uuid::new_v4()));
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
    let request = RequestId::new();
    TimelineRequestIngress {
        timeline: Arc::clone(&fixture.timeline),
    }
    .admit_same_branch_regenerate(fixture.source, fixture.fence, request, orphan_settings())
    .unwrap();
    let settings = Arc::clone(&fixture.settings_owner);
    let presets = Arc::clone(&fixture.presets);
    drop(fixture);
    let (a, a_timeline, _) = restart_api(
        &db,
        session,
        root,
        Arc::clone(&settings),
        Arc::clone(&presets),
    );
    let (b, b_timeline, _) = restart_api(&db, session, root, settings, presets);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let (first, second) = std::thread::scope(|scope| {
        let x = Arc::clone(&barrier);
        let a = scope.spawn(move || {
            x.wait();
            a.abandon_orphaned_same_branch_regenerate(root, request)
        });
        let y = Arc::clone(&barrier);
        let b = scope.spawn(move || {
            y.wait();
            b.abandon_orphaned_same_branch_regenerate(root, request)
        });
        barrier.wait();
        (a.join().unwrap(), b.join().unwrap())
    });
    let final_timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let final_state = final_timeline
        .reconcile_same_branch_regenerate(root, request)
        .unwrap()
        .outcome;
    assert!(matches!(
        final_state,
        RegenerateRecoveryOutcome::Abandoned(_)
    ));
    for result in [first, second] {
        match result {
            Ok(outcome) => assert_eq!(outcome, final_state),
            Err(TimelineError::ConcurrentWrite) => {}
            other => panic!("unexpected dual-owner orphan result: {other:?}"),
        }
    }
    let snapshot = crate::timeline::storage::SqliteTimelineRepository::open(&db)
        .unwrap()
        .archive_snapshot()
        .unwrap();
    assert_eq!(
        snapshot
            .world(root)
            .unwrap()
            .iter()
            .filter(
                |fact| fact.draft.kind == crate::timeline::domain::EventKind::RegenerateAbandoned
            )
            .count(),
        1
    );
    drop((a_timeline, b_timeline, final_timeline));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}
