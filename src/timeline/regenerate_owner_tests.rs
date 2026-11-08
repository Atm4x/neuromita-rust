use std::sync::Arc;

use serde_json::json;

use super::*;

fn accepted_source(timeline: &mut SessionTimeline, character: CharacterId) -> RegenerateSource {
    let original_input = EventId::new();
    timeline
        .publish_player_message(character, original_input, Arc::from("original input"))
        .unwrap();
    let accepted_request = RequestId::new();
    let mut reply = ConversationMessage::new(
        EventId::new().0.to_string(),
        ConversationSpeaker::Character(character),
        json!("accepted reply"),
        [character],
    );
    let accepted_message = reply.id;
    reply.request_id = Some(accepted_request);
    reply.reply_to = Some(original_input);
    reply.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: original_input,
    });
    timeline
        .publish_conversation_message(timeline.selection(), reply)
        .unwrap();
    RegenerateSource {
        character,
        accepted_message,
        accepted_request,
        original_input,
    }
}

#[test]
fn accepted_source_fork_preserves_original_fact_and_excludes_accepted_reply() {
    let mut timeline = SessionTimeline::new();
    let parent = timeline.branch;
    let source = accepted_source(&mut timeline, CharacterId::Crazy);
    let original = timeline.store.event(parent, source.original_input).unwrap();
    let before = timeline.store.world(parent).unwrap();
    let selected = timeline.selection();
    let child = BranchId::new();
    let chosen = timeline
        .fork_and_select_regenerate_source(source, child, selected, 2)
        .unwrap();
    assert_eq!(chosen.generation, selected.generation + 1);
    assert_eq!(timeline.store.world(parent).unwrap(), before);
    assert_eq!(
        timeline.store.event(child, source.original_input).unwrap(),
        original
    );
    assert_eq!(
        timeline.store.event(child, source.accepted_message),
        Err(domain::TimelineError::NotFound)
    );
    assert_eq!(timeline.anchor().through, 2);
    assert_eq!(
        timeline
            .store
            .knows(child, CharacterId::Kind, source.original_input, None),
        Ok(false)
    );
}

#[test]
fn forged_request_input_character_and_fence_leave_timeline_unchanged() {
    let mut timeline = SessionTimeline::new();
    let source = accepted_source(&mut timeline, CharacterId::Crazy);
    let selected = timeline.selection();
    let before = timeline.store.world(timeline.branch).unwrap();
    for forged in [
        RegenerateSource {
            accepted_request: RequestId::new(),
            ..source
        },
        RegenerateSource {
            accepted_message: EventId::new(),
            ..source
        },
        RegenerateSource {
            original_input: EventId::new(),
            ..source
        },
        RegenerateSource {
            character: CharacterId::Kind,
            ..source
        },
    ] {
        let child = BranchId::new();
        assert!(
            timeline
                .fork_and_select_regenerate_source(forged, child, selected, 2)
                .is_err()
        );
        assert_eq!(
            timeline.store.branch(child),
            Err(domain::TimelineError::NotFound)
        );
        assert_eq!(timeline.selection(), selected);
    }
    let stale = BranchId::new();
    assert_eq!(
        timeline.fork_and_select_regenerate_source(source, stale, selected, 1),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    assert_eq!(
        timeline.store.branch(stale),
        Err(domain::TimelineError::NotFound)
    );
    assert_eq!(timeline.store.world(timeline.branch).unwrap(), before);
}

#[test]
fn hidden_reply_hidden_input_and_superseding_reply_are_rejected() {
    for hidden in [false, true] {
        let mut timeline = SessionTimeline::new();
        let source = accepted_source(&mut timeline, CharacterId::Crazy);
        let mut hide = EventDraft::new(EventKind::MessageHidden);
        hide.observers.insert(CharacterId::Crazy);
        hide.payload.insert(
            "event_ids".into(),
            json!([if hidden {
                source.original_input
            } else {
                source.accepted_message
            }
            .0
            .to_string()]),
        );
        timeline.append_test_fact("hide", hide).unwrap();
        let child = BranchId::new();
        assert!(
            timeline
                .fork_and_select_regenerate_source(source, child, timeline.selection(), 3)
                .is_err()
        );
        assert_eq!(
            timeline.store.branch(child),
            Err(domain::TimelineError::NotFound)
        );
    }
    let mut timeline = SessionTimeline::new();
    let first = accepted_source(&mut timeline, CharacterId::Crazy);
    let _second = accepted_source(&mut timeline, CharacterId::Crazy);
    let child = BranchId::new();
    assert!(
        timeline
            .fork_and_select_regenerate_source(first, child, timeline.selection(), 4)
            .is_err()
    );
    assert_eq!(
        timeline.store.branch(child),
        Err(domain::TimelineError::NotFound)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn canonical_receipt_fork_commits_to_sqlite_and_old_selection_cannot_replay() {
    let db =
        std::env::temp_dir().join(format!("neuromita-b60-receipt-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let source = accepted_source(&mut timeline, CharacterId::Crazy);
    let initial = timeline.selection();
    let child = BranchId::new();
    let chosen = timeline
        .fork_and_select_regenerate_source(source, child, initial, 2)
        .unwrap();
    drop(timeline);
    let mut reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.selection(), chosen);
    assert_eq!(
        reopened
            .store
            .event(child, source.original_input)
            .unwrap()
            .draft
            .id,
        source.original_input
    );
    assert_eq!(
        reopened.store.event(child, source.accepted_message),
        Err(domain::TimelineError::NotFound)
    );
    let back = reopened.select_branch(root, chosen).unwrap();
    let stale = BranchId::new();
    assert_eq!(
        reopened.fork_and_select_regenerate_source(source, stale, initial, 2),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    assert_eq!(
        reopened.store.branch(stale),
        Err(domain::TimelineError::NotFound)
    );
    assert_eq!(reopened.selection(), back);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[test]
fn request_id_alone_does_not_authorize_a_forged_causal_receipt() {
    for (has_link, has_payload) in [(false, true), (true, false)] {
        let mut timeline = SessionTimeline::new();
        let input = EventId::new();
        timeline
            .publish_player_message(CharacterId::Crazy, input, Arc::from("original"))
            .unwrap();
        let request = RequestId::new();
        let mut reply = ConversationMessage::new(
            EventId::new().0.to_string(),
            ConversationSpeaker::Character(CharacterId::Crazy),
            json!("forged receipt"),
            [CharacterId::Crazy],
        );
        reply.request_id = Some(request);
        if has_payload {
            reply.reply_to = Some(input);
        }
        if has_link {
            reply.links.push(CausalLink {
                relation: CausalRelation::ReplyTo,
                event_id: input,
            });
        }
        let source = RegenerateSource {
            character: CharacterId::Crazy,
            accepted_message: reply.id,
            accepted_request: request,
            original_input: input,
        };
        timeline
            .publish_conversation_message(timeline.selection(), reply)
            .unwrap();
        let before = timeline.store.world(timeline.branch).unwrap();
        let selection = timeline.selection();
        let child = BranchId::new();
        assert_eq!(
            timeline.fork_and_select_regenerate_source(source, child, selection, 2),
            Err(domain::TimelineError::Invalid(
                "regenerate receipt does not refer to original player input"
            ))
        );
        assert_eq!(
            timeline.store.branch(child),
            Err(domain::TimelineError::NotFound)
        );
        assert_eq!(timeline.store.world(timeline.branch).unwrap(), before);
        assert_eq!(timeline.selection(), selection);
    }
}

#[test]
fn duplicate_durable_request_id_rejected_before_second_fork_mutates_owner() {
    let mut timeline = SessionTimeline::new();
    let parent = timeline.selection().branch_id;
    let source = accepted_source(&mut timeline, CharacterId::Crazy);
    let first_selection = timeline.selection();
    let first = BranchId::new();
    let request = RequestId::new();
    let mut receipt = RegenerateAdmissionReceipt {
        request,
        accepted_request: source.accepted_request,
        accepted_message: source.accepted_message,
        original_input: source.original_input,
        character: source.character,
        parent_branch: parent,
        parent_head: 2,
        parent_generation: first_selection.generation,
        child_branch: first,
        configuration: RegenerateConfigurationReference {
            settings_revision: 0,
            preset_id: 0,
            provider_model: "model".into(),
            prompt_set: None,
        },
    };
    let selected = timeline
        .fork_and_admit_regenerate_source(source, first, first_selection, 2, &receipt)
        .unwrap();
    let returned = timeline.select_branch(parent, selected.selection).unwrap();
    let second = BranchId::new();
    receipt.child_branch = second;
    receipt.parent_generation = returned.generation;
    let before = timeline.store.world(parent).unwrap();
    assert_eq!(
        timeline.fork_and_admit_regenerate_source(source, second, returned, 2, &receipt),
        Err(domain::TimelineError::Conflict),
    );
    assert_eq!(timeline.selection(), returned);
    assert_eq!(timeline.store.world(parent).unwrap(), before);
    assert_eq!(
        timeline.store.branch(second),
        Err(domain::TimelineError::NotFound)
    );
}

fn admitted_recovery_source(
    timeline: &mut SessionTimeline,
) -> (RegenerateSource, BranchId, RequestId) {
    let source = accepted_source(timeline, CharacterId::Crazy);
    let expected = timeline.selection();
    let child = BranchId::new();
    let request = RequestId::new();
    let receipt = RegenerateAdmissionReceipt {
        request,
        accepted_request: source.accepted_request,
        accepted_message: source.accepted_message,
        original_input: source.original_input,
        character: source.character,
        parent_branch: expected.branch_id,
        parent_head: 2,
        parent_generation: expected.generation,
        child_branch: child,
        configuration: RegenerateConfigurationReference {
            settings_revision: 7,
            preset_id: 2,
            provider_model: "recovery-model".into(),
            prompt_set: Some("frozen-reference".into()),
        },
    };
    timeline
        .fork_and_admit_regenerate_source(source, child, expected, 2, &receipt)
        .unwrap();
    (source, child, request)
}

#[test]
fn reconcile_unknown_dispatch_is_read_only_and_not_a_replay_permit() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) = admitted_recovery_source(&mut timeline);
    let prior = timeline.store.world(child).unwrap();
    let selection = timeline.selection();
    let snapshot = timeline.reconcile_regenerate(child, request).unwrap();
    assert_eq!(snapshot.admission.original_input, source.original_input);
    assert_eq!(snapshot.selection, selection);
    assert_eq!(snapshot.outcome, RegenerateRecoveryOutcome::DispatchUnknown);
    assert_eq!(
        timeline.reconcile_regenerate(child, request).unwrap(),
        snapshot
    );
    assert_eq!(timeline.store.world(child).unwrap(), prior);
    assert_eq!(timeline.selection(), selection);
    assert_eq!(
        timeline.reconcile_regenerate(child, RequestId::new()),
        Err(domain::TimelineError::Conflict)
    );
    assert_eq!(
        timeline.reconcile_regenerate(selection.branch_id, RequestId::new()),
        Err(domain::TimelineError::Conflict)
    );
}

#[test]
fn reconcile_accepted_reply_checks_causal_identity_and_multiple_replies() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) = admitted_recovery_source(&mut timeline);
    let mut response = ConversationMessage::new(
        EventId::new().0.to_string(),
        ConversationSpeaker::Character(source.character),
        json!("recovered reply"),
        [source.character],
    );
    response.request_id = Some(request);
    response.reply_to = Some(source.original_input);
    response.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: source.original_input,
    });
    let message = response.id;
    timeline
        .publish_conversation_message(timeline.selection(), response)
        .unwrap();
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::AcceptedReply(message)
    );
    let mut duplicate = ConversationMessage::new(
        EventId::new().0.to_string(),
        ConversationSpeaker::Character(source.character),
        json!("duplicate"),
        [source.character],
    );
    duplicate.request_id = Some(request);
    duplicate.reply_to = Some(source.original_input);
    duplicate.links.push(CausalLink {
        relation: CausalRelation::ReplyTo,
        event_id: source.original_input,
    });
    timeline
        .publish_conversation_message(timeline.selection(), duplicate)
        .unwrap();
    assert_eq!(
        timeline.reconcile_regenerate(child, request),
        Err(domain::TimelineError::Conflict)
    );
}

#[test]
fn reconcile_rejects_request_id_with_wrong_causal_reply() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) = admitted_recovery_source(&mut timeline);
    let mut forged = ConversationMessage::new(
        EventId::new().0.to_string(),
        ConversationSpeaker::Character(source.character),
        json!("unrelated reply"),
        [source.character],
    );
    forged.request_id = Some(request);
    timeline
        .publish_conversation_message(timeline.selection(), forged)
        .unwrap();
    assert_eq!(
        timeline.reconcile_regenerate(child, request),
        Err(domain::TimelineError::Invalid(
            "regenerate acceptance has invalid causal source"
        ))
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_reconcile_survives_restart_and_distinguishes_aba_selection() {
    let db = std::env::temp_dir().join(format!("b63-recovery-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let (source, child, request) = admitted_recovery_source(&mut timeline);
    let selected = timeline.selection();
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    drop(timeline);
    let mut reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let after_restart = reopened.reconcile_regenerate(child, request).unwrap();
    assert_eq!(
        after_restart.admission.original_input,
        source.original_input
    );
    assert_eq!(after_restart.selection, selected);
    let back = reopened.select_branch(root, selected).unwrap();
    let not_selected = reopened.reconcile_regenerate(child, request).unwrap();
    assert_eq!(not_selected.selection, back);
    assert_ne!(not_selected.selection.branch_id, child);
    assert_eq!(
        not_selected.outcome,
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    let returned = reopened.select_branch(child, back).unwrap();
    let aba = reopened.reconcile_regenerate(child, request).unwrap();
    assert_eq!(aba.selection, returned);
    assert!(aba.selection.generation > selected.generation);
    assert_eq!(aba.outcome, RegenerateRecoveryOutcome::DispatchUnknown);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[test]
fn generated_reply_cannot_reuse_regenerate_request_for_another_intent() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) = admitted_recovery_source(&mut timeline);
    let pinned = timeline
        .capture_turn(
            source.character,
            Some(source.original_input),
            timeline.anchor(),
        )
        .unwrap();
    let prior = timeline.store.world(child).unwrap();
    let forged = timeline.append_reply(GeneratedReply {
        request,
        intent: TurnIntent::React(source.original_input),
        turn: pinned,
        expected_revision: 0,
        content: json!("forged reaction"),
        audience: ConversationAudience::private_to(source.character),
        effects: Vec::new(),
    });
    assert_eq!(
        forged,
        Err(domain::TimelineError::Invalid(
            "regenerate acceptance violates durable admission"
        ))
    );
    assert_eq!(timeline.store.world(child).unwrap(), prior);
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::DispatchUnknown
    );
}

#[test]
fn generated_reply_accepts_once_and_reconciliation_uses_same_receipt() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) = admitted_recovery_source(&mut timeline);
    let pinned = timeline
        .capture_turn(
            source.character,
            Some(source.original_input),
            timeline.anchor(),
        )
        .unwrap();
    let reply = || GeneratedReply {
        request,
        intent: TurnIntent::Reply(source.original_input),
        turn: pinned,
        expected_revision: 0,
        content: json!("regenerated"),
        audience: ConversationAudience::private_to(source.character),
        effects: Vec::new(),
    };
    let accepted = timeline.append_reply(reply()).unwrap();
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    let facts = timeline.store.world(child).unwrap();
    assert_eq!(
        timeline.append_reply(reply()),
        Err(domain::TimelineError::Conflict)
    );
    assert_eq!(timeline.store.world(child).unwrap(), facts);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_competing_reply_writers_accept_one_durable_regenerate_result() {
    let db = std::env::temp_dir().join(format!("b63-race-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut initial = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let (source, child, request) = admitted_recovery_source(&mut initial);
    drop(initial);
    let gate = Arc::new(std::sync::Barrier::new(3));
    let outcomes = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let database = db.clone();
            let gate = Arc::clone(&gate);
            handles.push(scope.spawn(move || {
                let mut owner = SessionTimeline::from_config(TimelineConfig::Sqlite {
                    database,
                    session,
                    root,
                })
                .unwrap();
                let turn = owner
                    .capture_turn(
                        source.character,
                        Some(source.original_input),
                        owner.anchor(),
                    )
                    .unwrap();
                gate.wait();
                owner.append_reply(GeneratedReply {
                    request,
                    intent: TurnIntent::Reply(source.original_input),
                    turn,
                    expected_revision: 0,
                    content: json!("concurrent generated answer"),
                    audience: ConversationAudience::private_to(source.character),
                    effects: Vec::new(),
                })
            }));
        }
        gate.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_err()).count(),
        1
    );
    let accepted = outcomes.into_iter().find_map(Result::ok).unwrap();
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
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    assert_eq!(reopened.store.branch(child).unwrap().head_sequence, 3);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}
