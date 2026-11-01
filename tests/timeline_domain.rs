use neuromita::{
    character::CharacterId,
    timeline::domain::{
        BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId, TimelineError,
        TimelineStore,
    },
};
use serde_json::{Value, json};

fn setup() -> (TimelineStore, BranchId) {
    let mut timeline = TimelineStore::new();
    let branch = BranchId::new();
    timeline.create_session(SessionId::new(), branch).unwrap();
    (timeline, branch)
}

fn fact(kind: EventKind, character: CharacterId) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.observers.insert(character);
    draft
}

fn push(timeline: &mut TimelineStore, branch: BranchId, key: &str, drafts: Vec<EventDraft>) {
    let head = timeline.branch(branch).unwrap().head_sequence;
    timeline
        .append(branch, head, key, json!({"key": key}), drafts)
        .unwrap();
}

#[test]
fn branch_prefix_is_retained_without_inheriting_parent_future() {
    let (mut timeline, root) = setup();
    let checkpoint = EventDraft {
        payload: json!({"snapshot": {"world": {"lives": 3}}})
            .as_object()
            .unwrap()
            .clone(),
        ..EventDraft::new(EventKind::Checkpoint)
    };
    let checkpoint_id = checkpoint.id;
    push(&mut timeline, root, "checkpoint", vec![checkpoint]);
    let late = fact(EventKind::UnityEvent, CharacterId::Mita);
    let late_id = late.id;
    push(&mut timeline, root, "future", vec![late]);
    let child = BranchId::new();
    let info = timeline.fork(root, checkpoint_id, child).unwrap();
    assert_eq!(info.fork_sequence, 1);
    assert_eq!(info.head_sequence, 2);
    assert_eq!(timeline.world(child).unwrap().len(), 2);
    assert_eq!(timeline.event(child, late_id), Err(TimelineError::NotFound));
    assert!(timeline.event(root, late_id).is_ok());
    let nested_checkpoint = EventDraft {
        payload: json!({"snapshot": {}}).as_object().unwrap().clone(),
        ..EventDraft::new(EventKind::Checkpoint)
    };
    let second_checkpoint = nested_checkpoint.id;
    push(
        &mut timeline,
        child,
        "checkpoint-nested",
        vec![nested_checkpoint],
    );
    let nested = BranchId::new();
    timeline.fork(child, second_checkpoint, nested).unwrap();
    assert!(timeline.event(nested, late_id).is_err());
    let sequences: Vec<_> = timeline
        .world(nested)
        .unwrap()
        .iter()
        .map(|e| e.sequence)
        .collect();
    assert_eq!(sequences, vec![1, 2, 3, 4]);
}

#[test]
fn retry_returns_exact_receipt_even_after_head_advanced() {
    let (mut timeline, root) = setup();
    let draft = fact(EventKind::UnityEvent, CharacterId::Mita);
    let original = timeline
        .append(root, 0, "first", json!({}), vec![draft.clone()])
        .unwrap();
    push(
        &mut timeline,
        root,
        "second",
        vec![fact(EventKind::UnityEvent, CharacterId::Mita)],
    );
    let mut replay = draft.clone();
    replay.occurred_at_unix_ms = 456789;
    assert_eq!(
        timeline
            .append(root, 0, "first", json!({}), vec![replay])
            .unwrap(),
        original
    );
    let mut conflicting = draft;
    conflicting.payload.insert("other".into(), json!(1));
    assert_eq!(
        timeline.append(root, 0, "first", json!({}), vec![conflicting]),
        Err(TimelineError::Conflict)
    );
    assert_eq!(timeline.branch(root).unwrap().head_sequence, 2);
}

#[test]
fn concurrent_expected_head_is_checked_and_batch_is_atomic() {
    let (mut timeline, root) = setup();
    let known = fact(EventKind::UnityEvent, CharacterId::Mita);
    push(&mut timeline, root, "first", vec![known]);
    assert_eq!(
        timeline.append(root, 0, "stale", json!({}), vec![]),
        Err(TimelineError::ConcurrentWrite)
    );
    let valid = fact(EventKind::UnityEvent, CharacterId::Mita);
    let invalid = EventDraft::new(EventKind::CharacterMessage);
    assert!(
        timeline
            .append(root, 1, "bad", json!({}), vec![valid, invalid])
            .is_err()
    );
    assert_eq!(timeline.branch(root).unwrap().head_sequence, 1);
    assert_eq!(timeline.world(root).unwrap().len(), 1);
}

#[test]
fn empty_batch_gets_receipt_without_inventing_event() {
    let (mut timeline, root) = setup();
    assert_eq!(
        timeline
            .append(root, 0, "empty", json!({}), vec![])
            .unwrap(),
        vec![]
    );
    push(
        &mut timeline,
        root,
        "advance",
        vec![fact(EventKind::UnityEvent, CharacterId::Mita)],
    );
    assert_eq!(
        timeline
            .append(root, 0, "empty", json!({}), vec![])
            .unwrap(),
        vec![]
    );
    assert_eq!(timeline.branch(root).unwrap().head_sequence, 1);
}

#[test]
fn character_only_learns_private_event_by_explicit_disclosure() {
    let (mut timeline, root) = setup();
    let secret = fact(EventKind::UnityEvent, CharacterId::Mita);
    let secret_id = secret.id;
    push(&mut timeline, root, "secret", vec![secret]);
    assert!(
        !timeline
            .knows(root, CharacterId::KindMita, secret_id, None)
            .unwrap()
    );
    let mut disclosure = fact(EventKind::EventObserved, CharacterId::KindMita);
    disclosure.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: secret_id,
    });
    push(&mut timeline, root, "disclose", vec![disclosure]);
    let view = timeline.view(root, CharacterId::KindMita, None).unwrap();
    let revealed = view
        .iter()
        .find(|event| event.fact.draft.id == secret_id)
        .unwrap();
    assert_eq!(revealed.learned_at, 2);
    assert_eq!(revealed.fact.sequence, 1);
    assert!(
        !timeline
            .knows(root, CharacterId::KindMita, secret_id, Some(1))
            .unwrap()
    );
}

#[test]
fn context_reset_forgets_and_subsequent_disclosure_restores_knowledge() {
    let (mut timeline, root) = setup();
    let secret = fact(EventKind::UnityEvent, CharacterId::Mita);
    let id = secret.id;
    push(&mut timeline, root, "secret", vec![secret]);
    push(
        &mut timeline,
        root,
        "reset",
        vec![fact(EventKind::CharacterContextReset, CharacterId::Mita)],
    );
    assert!(!timeline.knows(root, CharacterId::Mita, id, None).unwrap());
    let mut disclose = fact(EventKind::EventObserved, CharacterId::Mita);
    disclose.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: id,
    });
    push(&mut timeline, root, "relearn", vec![disclose]);
    let view = timeline.view(root, CharacterId::Mita, None).unwrap();
    assert_eq!(
        view.iter()
            .find(|known| known.fact.draft.id == id)
            .unwrap()
            .learned_at,
        3
    );
}

#[test]
fn response_cannot_target_unknown_or_future_events() {
    let (mut timeline, root) = setup();
    let mut player = fact(EventKind::PlayerMessage, CharacterId::Mita);
    player.actor = Some(CharacterId::KindMita.to_string());
    let id = player.id;
    push(&mut timeline, root, "player", vec![player]);
    let mut response = fact(EventKind::CharacterMessage, CharacterId::KindMita);
    response.actor = Some(CharacterId::KindMita.to_string());
    response.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: id,
    });
    assert!(
        timeline
            .append(root, 1, "unknown", json!({}), vec![response.clone()])
            .is_err()
    );
    response.observers.insert(CharacterId::Mita);
    response.actor = Some(CharacterId::Mita.to_string());
    push(&mut timeline, root, "known", vec![response]);
    let mut missing = fact(EventKind::EventObserved, CharacterId::Mita);
    missing.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: neuromita::timeline::EventId::new(),
    });
    assert_eq!(
        timeline.append(root, 2, "future", json!({}), vec![missing]),
        Err(TimelineError::NotFound)
    );
}

#[test]
fn presence_changes_do_not_reveal_earlier_events() {
    let (mut timeline, root) = setup();
    let mut joined = fact(EventKind::CharacterJoined, CharacterId::Mita);
    joined.actor = Some(CharacterId::Mita.to_string());
    push(&mut timeline, root, "join", vec![joined]);
    let secret = fact(EventKind::UnityEvent, CharacterId::Mita);
    let secret_id = secret.id;
    push(&mut timeline, root, "secret", vec![secret]);
    let mut joined_later = fact(EventKind::CharacterJoined, CharacterId::KindMita);
    joined_later.actor = Some(CharacterId::KindMita.to_string());
    push(&mut timeline, root, "join-later", vec![joined_later]);
    assert!(
        timeline
            .present(root)
            .unwrap()
            .contains(&CharacterId::KindMita)
    );
    assert!(
        !timeline
            .knows(root, CharacterId::KindMita, secret_id, None)
            .unwrap()
    );
}

#[test]
fn external_identity_is_unique_within_inherited_world_but_not_sibling_futures() {
    let (mut timeline, root) = setup();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let cp = checkpoint.id;
    push(&mut timeline, root, "checkpoint", vec![checkpoint]);
    let child = BranchId::new();
    timeline.fork(root, cp, child).unwrap();
    let mut source = fact(EventKind::UnityEvent, CharacterId::Mita);
    source.external_message_id = Some("message-1".into());
    push(&mut timeline, root, "parent", vec![source]);
    let mut alternative = fact(EventKind::UnityEvent, CharacterId::Mita);
    alternative.external_message_id = Some("message-1".into());
    push(&mut timeline, child, "child", vec![alternative]);
    let mut duplicate = fact(EventKind::UnityEvent, CharacterId::Mita);
    duplicate.external_message_id = Some("message-1".into());
    assert_eq!(
        timeline.append(child, 3, "dup", json!({}), vec![duplicate]),
        Err(TimelineError::Conflict)
    );
}

#[test]
fn invalid_root_and_fork_identity_conflicts_do_not_mutate_state() {
    let (mut timeline, root) = setup();
    assert!(timeline.create_session(SessionId::new(), root).is_err());
    let mut cp = EventDraft::new(EventKind::Checkpoint);
    cp.payload.insert("snapshot".into(), json!({}));
    let cp_id = cp.id;
    push(&mut timeline, root, "cp", vec![cp]);
    let child = BranchId::new();
    let info = timeline.fork(root, cp_id, child).unwrap();
    assert_eq!(timeline.fork(root, cp_id, child).unwrap(), info);
    assert_eq!(
        timeline.fork(root, neuromita::timeline::EventId::new(), child),
        Err(TimelineError::Conflict)
    );
}

#[test]
fn empty_and_malformed_operation_commands_fail_before_mutation() {
    let (mut timeline, root) = setup();
    assert_eq!(
        timeline.append(root, 0, "", json!({}), vec![]),
        Err(TimelineError::Invalid(
            "operation key must be canonical and nonempty"
        ))
    );
    assert_eq!(
        timeline.append(root, 0, "valid", Value::Null, vec![]),
        Err(TimelineError::Invalid(
            "operation command must be an object"
        ))
    );
    assert_eq!(timeline.branch(root).unwrap().head_sequence, 0);
}

#[test]
fn message_edits_are_per_character_and_preserve_original_facts() {
    use neuromita::timeline::domain::character_messages;
    let (mut timeline, root) = setup();
    let mut message = fact(EventKind::PlayerMessage, CharacterId::Mita);
    message.actor = Some("player".into());
    message.observers.insert(CharacterId::KindMita);
    message.payload.insert("content".into(), json!("original"));
    let target = message.id;
    push(&mut timeline, root, "message", vec![message]);

    let mut hide = fact(EventKind::MessageHidden, CharacterId::Mita);
    hide.payload
        .insert("event_ids".into(), json!([target.0.to_string()]));
    push(&mut timeline, root, "hide", vec![hide]);
    let facts = timeline.world(root).unwrap();
    assert!(character_messages(&facts, CharacterId::Mita).is_empty());
    assert_eq!(
        character_messages(&facts, CharacterId::KindMita)[0].content,
        "original"
    );

    let mut revised = fact(EventKind::MessageRevised, CharacterId::Mita);
    revised
        .payload
        .insert("event_id".into(), json!(target.0.to_string()));
    revised
        .payload
        .insert("message".into(), json!({"content": "revised"}));
    push(&mut timeline, root, "revise", vec![revised]);
    let facts = timeline.world(root).unwrap();
    assert_eq!(
        character_messages(&facts, CharacterId::Mita)[0].content,
        "revised"
    );
    assert_eq!(
        character_messages(&facts, CharacterId::KindMita)[0].content,
        "original"
    );
    assert_eq!(
        timeline.event(root, target).unwrap().draft.payload["content"],
        json!("original")
    );

    let mut restore = fact(EventKind::MessageRestored, CharacterId::Mita);
    restore
        .payload
        .insert("event_id".into(), json!(target.0.to_string()));
    push(&mut timeline, root, "restore", vec![restore]);
    assert_eq!(
        character_messages(&timeline.world(root).unwrap(), CharacterId::Mita)[0].content,
        "revised"
    );
}

#[test]
fn message_edit_cannot_reveal_unknown_identity_or_change_author() {
    let (mut timeline, root) = setup();
    let mut secret = fact(EventKind::PlayerMessage, CharacterId::Mita);
    secret.actor = Some("player".to_string());
    secret.payload.insert("content".into(), json!("private"));
    let event_id = secret.id;
    push(&mut timeline, root, "secret", vec![secret]);

    let mut hidden = fact(EventKind::MessageHidden, CharacterId::KindMita);
    hidden
        .payload
        .insert("event_ids".into(), json!([event_id.0.to_string()]));
    assert_eq!(
        timeline.append(root, 1, "hide-secret", json!({}), vec![hidden]),
        Err(TimelineError::Invalid("cannot modify unknown message"))
    );

    let mut revision = fact(EventKind::MessageRevised, CharacterId::Mita);
    revision
        .payload
        .insert("event_id".into(), json!(event_id.0.to_string()));
    revision.payload.insert(
        "message".into(),
        json!({"content": "changed", "role": "assistant"}),
    );
    assert_eq!(
        timeline.append(root, 1, "invalid-revision", json!({}), vec![revision]),
        Err(TimelineError::Invalid(
            "revision cannot replace message identity"
        ))
    );
    assert_eq!(timeline.branch(root).unwrap().head_sequence, 1);
}

#[test]
fn context_reset_discards_message_projection_but_keeps_canonical_facts() {
    use neuromita::timeline::domain::character_messages;
    let (mut timeline, root) = setup();
    let mut message = fact(EventKind::PlayerMessage, CharacterId::Mita);
    message.actor = Some("player".to_string());
    message
        .payload
        .insert("content".into(), json!("before reset"));
    push(&mut timeline, root, "message", vec![message]);
    push(
        &mut timeline,
        root,
        "reset",
        vec![fact(EventKind::CharacterContextReset, CharacterId::Mita)],
    );
    assert_eq!(timeline.world(root).unwrap().len(), 2);
    assert!(character_messages(&timeline.world(root).unwrap(), CharacterId::Mita).is_empty());
}

#[test]
fn duplicates_inside_same_batch_roll_back_atomically() {
    let (mut timeline, root) = setup();
    let first = fact(EventKind::UnityEvent, CharacterId::Mita);
    let second = first.clone();
    assert_eq!(
        timeline.append(root, 0, "duplicates", json!({}), vec![first, second]),
        Err(TimelineError::Conflict)
    );
    assert_eq!(timeline.branch(root).unwrap().head_sequence, 0);
}

#[test]
fn invalid_causality_cannot_point_to_new_sibling_future() {
    let (mut timeline, root) = setup();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let id = checkpoint.id;
    push(&mut timeline, root, "checkpoint", vec![checkpoint]);
    let child = BranchId::new();
    timeline.fork(root, id, child).unwrap();
    let source = fact(EventKind::UnityEvent, CharacterId::Mita);
    let future = source.id;
    push(&mut timeline, root, "parent-future", vec![source]);
    let mut caused = fact(EventKind::UnityEvent, CharacterId::Mita);
    caused.links.push(CausalLink {
        relation: CausalRelation::CausedBy,
        event_id: future,
    });
    assert_eq!(
        timeline.append(child, 2, "invalid-causality", json!({}), vec![caused]),
        Err(TimelineError::NotFound)
    );
}
