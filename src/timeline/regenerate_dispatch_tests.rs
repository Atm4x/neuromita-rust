use std::sync::Arc;

use serde_json::json;

use super::*;

#[test]
fn dispatch_permit_is_once_and_recovery_cannot_trigger_replay() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let selected = timeline.selection();
    let before = timeline.store.world(child).unwrap();
    let permitted = timeline
        .permit_regenerate_dispatch(child, request, selected)
        .unwrap();
    assert_eq!(timeline.selection(), selected);
    assert_eq!(timeline.store.world(child).unwrap().len(), before.len() + 1);
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::MayHaveDispatched(permitted)
    );
    let persisted = timeline.store.world(child).unwrap();
    assert_eq!(
        timeline.permit_regenerate_dispatch(child, request, selected),
        Err(domain::TimelineError::Conflict)
    );
    assert_eq!(
        timeline.permit_regenerate_dispatch(child, RequestId::new(), selected),
        Err(domain::TimelineError::Conflict)
    );
    assert_eq!(timeline.store.world(child).unwrap(), persisted);
    let abandoned = timeline
        .abandon_regenerate(child, request, selected)
        .unwrap();
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::Abandoned(abandoned)
    );
    assert_eq!(
        timeline.append_reply(GeneratedReply {
            request,
            intent: TurnIntent::Reply(source.original_input),
            turn: timeline
                .capture_turn(
                    source.character,
                    Some(source.original_input),
                    timeline.anchor()
                )
                .unwrap(),
            expected_revision: 0,
            content: json!("too late"),
            audience: ConversationAudience::private_to(source.character),
            effects: Vec::new(),
        }),
        Err(domain::TimelineError::Conflict)
    );
}

#[test]
fn permitted_dispatch_can_accept_one_canonical_reply_and_no_second_dispatch() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let pinned = timeline
        .capture_turn(
            source.character,
            Some(source.original_input),
            timeline.anchor(),
        )
        .unwrap();
    let selection = timeline.selection();
    timeline
        .permit_regenerate_dispatch(child, request, selection)
        .unwrap();
    let accepted = timeline
        .append_reply(GeneratedReply {
            request,
            intent: TurnIntent::Reply(source.original_input),
            turn: pinned,
            expected_revision: 0,
            content: json!("canonical regenerated answer"),
            audience: ConversationAudience::private_to(source.character),
            effects: Vec::new(),
        })
        .unwrap();
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    assert_eq!(
        timeline.permit_regenerate_dispatch(child, request, selection),
        Err(domain::TimelineError::Conflict)
    );
    assert_eq!(timeline.store.branch(child).unwrap().head_sequence, 4);
}

#[test]
fn abandoned_and_stale_selection_cannot_grant_dispatch() {
    let mut timeline = SessionTimeline::new();
    let (_, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let selected = timeline.selection();
    let root = timeline.store.branch(child).unwrap().parent.unwrap();
    let changed = timeline.select_branch(root, selected).unwrap();
    assert_eq!(
        timeline.permit_regenerate_dispatch(child, request, selected),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    assert_eq!(
        timeline.permit_regenerate_dispatch(child, request, changed),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    let back = timeline.select_branch(child, changed).unwrap();
    timeline.abandon_regenerate(child, request, back).unwrap();
    let before = timeline.store.world(child).unwrap();
    assert_eq!(
        timeline.permit_regenerate_dispatch(child, request, back),
        Err(domain::TimelineError::Conflict)
    );
    assert_eq!(timeline.store.world(child).unwrap(), before);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn dispatch_fence_survives_sqlite_reopen_and_aba() {
    let db = std::env::temp_dir().join(format!("b65-dispatch-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let config = TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    };
    let mut timeline = SessionTimeline::from_config(config).unwrap();
    let (_, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let first = timeline.selection();
    let permitted = timeline
        .permit_regenerate_dispatch(child, request, first)
        .unwrap();
    drop(timeline);
    let mut reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(
        reopened
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::MayHaveDispatched(permitted)
    );
    assert_eq!(
        reopened.permit_regenerate_dispatch(child, request, first),
        Err(domain::TimelineError::Conflict)
    );
    let moved = reopened.select_branch(root, first).unwrap();
    let back = reopened.select_branch(child, moved).unwrap();
    assert_eq!(
        reopened.permit_regenerate_dispatch(child, request, first),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    assert_eq!(
        reopened.permit_regenerate_dispatch(child, request, back),
        Err(domain::TimelineError::Conflict)
    );
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn concurrent_sqlite_dispatch_permits_cannot_both_succeed() {
    let db = std::env::temp_dir().join(format!("b65-dispatch-race-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut initial = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let (_, child, request) = super::regenerate_owner_tests::admitted_recovery_source(&mut initial);
    drop(initial);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let results = std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|_| {
                let db = db.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let mut owner = SessionTimeline::from_config(TimelineConfig::Sqlite {
                        database: db,
                        session,
                        root,
                    })
                    .unwrap();
                    let selection = owner.selection();
                    barrier.wait();
                    owner.permit_regenerate_dispatch(child, request, selection)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:?}"
    );
    let permitted = results.into_iter().find_map(Result::ok).unwrap();
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(
        reopened
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::MayHaveDispatched(permitted)
    );
    assert_eq!(reopened.store.branch(child).unwrap().head_sequence, 3);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn dispatch_marker_archive_roundtrip_and_forgery_refusal() {
    use crate::timeline::storage::{SqliteTimelineRepository, archive, assets::TimelineAssetStore};
    let db = std::env::temp_dir().join(format!("b65-dispatch-archive-{}.db", uuid::Uuid::new_v4()));
    let binary_root =
        std::env::temp_dir().join(format!("b65-dispatch-assets-{}", uuid::Uuid::new_v4()));
    let assets = TimelineAssetStore::new(&binary_root, 1024 * 1024).unwrap();
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let (_, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let marker = timeline
        .permit_regenerate_dispatch(child, request, timeline.selection())
        .unwrap();
    drop(timeline);
    let source = SqliteTimelineRepository::open(&db).unwrap();
    let bytes = archive::export_sqlite(&source, session, &assets).unwrap();
    let mut restored = TimelineStore::new();
    archive::import_memory(&mut restored, &bytes, &assets).unwrap();
    assert_eq!(
        domain::dispatch_may_have_started(&restored.world(child).unwrap(), request.0)
            .unwrap()
            .unwrap()
            .draft
            .id,
        marker
    );
    let mut forged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let branch = forged["branches"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["info"]["id"] == json!(child))
        .unwrap();
    let foreign = RequestId::new().0.to_string();
    let fact = branch["events"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|fact| fact["draft"]["kind"] == "RegenerateMayHaveDispatched")
        .unwrap();
    fact["draft"]["payload"]["request_id"] = json!(foreign);
    let receipt = branch["receipts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["operation_key"] == format!("regenerate-dispatch:{}", request.0))
        .unwrap();
    receipt["drafts"][0]["payload"]["request_id"] = json!(foreign);
    receipt["result"][0]["draft"]["payload"]["request_id"] = json!(foreign);
    let mut rejected = TimelineStore::new();
    assert!(
        archive::import_memory(
            &mut rejected,
            &serde_json::to_vec(&forged).unwrap(),
            &assets
        )
        .is_err()
    );
    assert!(rejected.persisted_branches().is_empty());
    drop(source);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
    std::fs::remove_dir_all(binary_root).unwrap();
}
