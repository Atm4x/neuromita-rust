use super::*;
use serde_json::json;

#[test]
fn transient_historical_input_fork_keeps_canonical_input_and_excludes_downstream() {
    let mut timeline = SessionTimeline::new();
    let parent = timeline.branch;
    let selected = timeline.selection();
    let input = EventId::new();
    timeline
        .publish_player_message(
            CharacterId::Crazy,
            input,
            Arc::from("original with media identity"),
        )
        .unwrap();
    let original = timeline.store.event(parent, input).unwrap();
    let mut effect = EventDraft::new(EventKind::CharacterContextReset);
    effect.observers.insert(CharacterId::Kind);
    timeline
        .append_test_fact("downstream-reset", effect)
        .unwrap();
    let before = timeline.store.world(parent).unwrap();
    let child = BranchId::new();
    let after = timeline
        .fork_and_select_player_input(input, CharacterId::Crazy, child, selected, 2)
        .unwrap();
    assert_eq!(after.generation, selected.generation + 1);
    assert_eq!(timeline.anchor().through, 2);
    assert_eq!(timeline.store.world(parent).unwrap(), before);
    assert_eq!(timeline.store.event(child, input).unwrap(), original);
    assert_eq!(timeline.store.world(child).unwrap().len(), 2);
    assert_eq!(
        timeline.store.context_epoch(child, CharacterId::Crazy),
        Ok(0)
    );
    assert_eq!(
        timeline.store.context_epoch(parent, CharacterId::Crazy),
        Ok(0)
    );
    assert_eq!(
        timeline.store.context_epoch(parent, CharacterId::Kind),
        Ok(2)
    );
    assert_eq!(
        timeline.store.context_epoch(child, CharacterId::Kind),
        Ok(0)
    );
    assert_eq!(
        timeline.store.knows(child, CharacterId::Kind, input, None),
        Ok(false)
    );
    assert_eq!(
        timeline.store.knows(child, CharacterId::Crazy, input, None),
        Ok(true)
    );
    assert_eq!(
        timeline.store.branch(child).unwrap().checkpoint,
        Some(input)
    );
    assert_eq!(
        timeline.store.world(child).unwrap()[1].draft.payload["input_event_id"],
        json!(input.0.to_string())
    );
    let stale = BranchId::new();
    assert_eq!(
        timeline.fork_and_select_player_input(input, CharacterId::Crazy, stale, selected, 2),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    assert_eq!(
        timeline.store.branch(stale),
        Err(domain::TimelineError::NotFound)
    );
}

#[test]
fn historical_input_fork_rejects_foreign_non_input_hidden_and_stale_head() {
    let mut timeline = SessionTimeline::new();
    let selected = timeline.selection();
    let private = EventId::new();
    timeline
        .publish_player_message(CharacterId::Kind, private, Arc::from("secret"))
        .unwrap();
    let foreign_child = BranchId::new();
    assert_eq!(
        timeline.fork_and_select_player_input(
            private,
            CharacterId::Crazy,
            foreign_child,
            selected,
            1
        ),
        Err(domain::TimelineError::Invalid(
            "fork requires an addressed player input"
        ))
    );
    assert_eq!(
        timeline.store.branch(foreign_child),
        Err(domain::TimelineError::NotFound)
    );
    let not_input = EventDraft::new(EventKind::UnityEvent);
    let not_input_id = not_input.id;
    timeline.append_test_fact("not-player", not_input).unwrap();
    let fake_child = BranchId::new();
    assert_eq!(
        timeline.fork_and_select_player_input(
            not_input_id,
            CharacterId::Crazy,
            fake_child,
            selected,
            2
        ),
        Err(domain::TimelineError::Invalid(
            "fork requires an addressed player input"
        ))
    );
    let stale_child = BranchId::new();
    assert_eq!(
        timeline.fork_and_select_player_input(private, CharacterId::Kind, stale_child, selected, 1),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    let mut hide = EventDraft::new(EventKind::MessageHidden);
    hide.observers.insert(CharacterId::Kind);
    hide.payload
        .insert("event_ids".into(), json!([private.0.to_string()]));
    timeline.append_test_fact("hide", hide).unwrap();
    let hidden_child = BranchId::new();
    assert_eq!(
        timeline.fork_and_select_player_input(
            private,
            CharacterId::Kind,
            hidden_child,
            selected,
            3
        ),
        Err(domain::TimelineError::Invalid(
            "player input is not currently visible"
        ))
    );
    assert_eq!(
        timeline.store.branch(hidden_child),
        Err(domain::TimelineError::NotFound)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_historical_input_fork_reopens_and_aba_fence_fails() {
    let db = std::env::temp_dir().join(format!("neuromita-b59-input-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let selected = timeline.selection();
    let input = EventId::new();
    timeline
        .publish_player_message(CharacterId::Crazy, input, Arc::from("source"))
        .unwrap();
    let child = BranchId::new();
    let chosen = timeline
        .fork_and_select_player_input(input, CharacterId::Crazy, child, selected, 1)
        .unwrap();
    drop(timeline);
    let mut reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.selection(), chosen);
    assert_eq!(reopened.store.event(child, input).unwrap().draft.id, input);
    let back = reopened.select_branch(root, chosen).unwrap();
    assert_eq!(back.branch_id, root);
    let stale = BranchId::new();
    assert_eq!(
        reopened.fork_and_select_player_input(input, CharacterId::Crazy, stale, selected, 1),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    assert_eq!(
        reopened.store.branch(stale),
        Err(domain::TimelineError::NotFound)
    );
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[test]
fn native_archive_round_trip_preserves_input_fork_and_rejects_forged_recipient() {
    use crate::timeline::conversation::{ConversationMessage, ConversationSpeaker};
    use crate::timeline::storage::{archive, assets::TimelineAssetStore};
    let root_dir = std::env::temp_dir().join(format!("neuromita-b59-archive-{}", Uuid::new_v4()));
    let assets = TimelineAssetStore::new(&root_dir, 1024 * 1024).unwrap();
    let session = SessionId::new();
    let root = BranchId::new();
    let child = BranchId::new();
    let mut store = TimelineStore::new();
    store.create_session(session, root).unwrap();
    let input = ConversationMessage::new(
        "original",
        ConversationSpeaker::Player,
        json!("original"),
        [CharacterId::Crazy],
    );
    let original = input.id;
    store
        .append(
            root,
            0,
            "source",
            json!({}),
            vec![input.into_draft().unwrap()],
        )
        .unwrap();
    store
        .fork_at_player_input(root, original, CharacterId::Crazy, child)
        .unwrap();
    let bytes = archive::export_memory(&store, session, &assets).unwrap();
    let mut restored = TimelineStore::new();
    archive::import_memory(&mut restored, &bytes, &assets).unwrap();
    assert_eq!(restored.world(root).unwrap(), store.world(root).unwrap());
    assert_eq!(restored.world(child).unwrap(), store.world(child).unwrap());
    assert_eq!(
        restored.knows(child, CharacterId::Crazy, original, None),
        Ok(true)
    );
    assert_eq!(
        restored.knows(child, CharacterId::Kind, original, None),
        Ok(false)
    );
    let mut forged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let branch = forged["branches"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|branch| branch["info"]["id"] == json!(child))
        .unwrap();
    branch["events"][0]["draft"]["payload"]["character_id"] = json!(CharacterId::Kind.to_string());
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
    std::fs::remove_dir_all(root_dir).unwrap();
}
