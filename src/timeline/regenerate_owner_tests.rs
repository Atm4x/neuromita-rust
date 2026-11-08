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
