use super::*;
use serde_json::json;

fn message(timeline: &mut SessionTimeline, character: CharacterId, content: &str) -> EventId {
    let mut fact = EventDraft::new(EventKind::PlayerMessage);
    fact.actor = Some("player".into());
    fact.observers.insert(character);
    fact.payload.insert("content".into(), json!(content));
    let id = fact.id;
    timeline
        .append_test_fact(&format!("message:{id:?}"), fact)
        .unwrap();
    id
}

fn view(timeline: &SessionTimeline, character: CharacterId, at: u64) -> Vec<domain::KnownEvent> {
    timeline
        .store
        .view(timeline.branch, character, Some(at))
        .unwrap()
}

fn message_ids(timeline: &SessionTimeline, character: CharacterId, at: u64) -> Vec<EventId> {
    TimelineMessageQueries::capture(
        timeline.store.as_ref(),
        timeline.branch,
        character,
        Some(at),
    )
    .unwrap()
    .messages()
    .iter()
    .map(|message| message.event_id)
    .collect()
}

#[test]
fn revisioned_forget_reveal_is_not_retroactive_and_does_not_leak() {
    let mut timeline = SessionTimeline::new();
    let crazy = CharacterId::Crazy;
    let kind = CharacterId::Kind;
    let e1 = message(&mut timeline, crazy, "first");
    let e2 = message(&mut timeline, kind, "other mita private");
    let mut story = EventDraft::new(EventKind::CharacterMessage);
    story.actor = Some(kind.to_string());
    story.observers.extend([crazy, kind]);
    story
        .payload
        .insert("content".into(), json!("I heard about private event"));
    story.links.push(domain::CausalLink {
        relation: domain::CausalRelation::CausedBy,
        event_id: e2,
    });
    let story_id = story.id;
    timeline
        .append_test_fact("story:not-disclosure", story)
        .unwrap();
    assert_eq!(
        timeline.store.knows(timeline.branch, crazy, e2, None),
        Ok(false)
    );
    assert_eq!(
        timeline.store.knows(timeline.branch, crazy, story_id, None),
        Ok(true)
    );
    let before = view(&timeline, crazy, 3);
    assert!(before.iter().any(|known| known.fact.draft.id == e1));
    let first = timeline.selection();
    timeline
        .change_character_knowledge(first, crazy, EventId::new(), false, vec![e1])
        .unwrap();
    assert_eq!(
        timeline.store.knows(timeline.branch, crazy, e1, None),
        Ok(false)
    );
    assert_eq!(
        timeline.store.knows(timeline.branch, crazy, e1, Some(3)),
        Ok(true)
    );
    assert!(message_ids(&timeline, crazy, 4).iter().all(|id| *id != e1));
    let reveal = timeline
        .change_character_knowledge(first, crazy, EventId::new(), true, vec![e1, e2])
        .unwrap();
    let after = view(&timeline, crazy, 5);
    assert!(
        after
            .iter()
            .any(|known| known.fact.draft.id == e1 && known.learned_at == 5)
    );
    assert!(
        after
            .iter()
            .any(|known| known.fact.draft.id == e2 && known.learned_at == 5)
    );
    assert!(after.iter().any(|known| known.fact.draft.id == reveal));
    assert_eq!(message_ids(&timeline, crazy, 3).contains(&e2), false);
    assert!(message_ids(&timeline, crazy, 5).contains(&e2));
    assert!(message_ids(&timeline, kind, 5).contains(&e2));
    assert_eq!(
        timeline.store.knows(timeline.branch, kind, e1, None),
        Ok(false)
    );
    assert_eq!(timeline.store.context_epoch(timeline.branch, crazy), Ok(5));
    assert_eq!(timeline.store.context_epoch(timeline.branch, kind), Ok(0));
}

#[test]
fn history_reset_forgets_and_explicit_reveal_can_restore_exact_id() {
    let mut timeline = SessionTimeline::new();
    let e = message(&mut timeline, CharacterId::Crazy, "before reset");
    let other = message(&mut timeline, CharacterId::Kind, "other private");
    timeline
        .reset_character_context(timeline.selection(), CharacterId::Crazy, EventId::new())
        .unwrap();
    assert_eq!(
        message_ids(&timeline, CharacterId::Crazy, 3),
        Vec::<EventId>::new()
    );
    assert!(
        timeline
            .store
            .knows(timeline.branch, CharacterId::Kind, other, None)
            .unwrap()
    );
    let new_input = message(&mut timeline, CharacterId::Crazy, "after reset");
    assert_eq!(
        message_ids(&timeline, CharacterId::Crazy, 4),
        vec![new_input]
    );
    timeline
        .change_character_knowledge(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            true,
            vec![e],
        )
        .unwrap();
    assert_eq!(
        message_ids(&timeline, CharacterId::Crazy, 5),
        vec![new_input, e]
    );
    assert_eq!(message_ids(&timeline, CharacterId::Crazy, 2), vec![e]);
    assert!(!message_ids(&timeline, CharacterId::Kind, 5).contains(&e));
}

#[test]
fn invalid_control_rejects_without_any_mutation() {
    let mut timeline = SessionTimeline::new();
    let e = message(&mut timeline, CharacterId::Kind, "kind only");
    let original = timeline.store.world(timeline.branch).unwrap();
    let selected = timeline.selection();
    assert!(
        timeline
            .change_character_knowledge(
                selected,
                CharacterId::Crazy,
                EventId::new(),
                false,
                vec![e]
            )
            .is_err()
    );
    assert!(
        timeline
            .change_character_knowledge(
                selected,
                CharacterId::Crazy,
                EventId::new(),
                true,
                vec![e, e]
            )
            .is_err()
    );
    assert!(
        timeline
            .change_character_knowledge(
                selected,
                CharacterId::Crazy,
                EventId::new(),
                true,
                vec![EventId::new()]
            )
            .is_err()
    );
    assert!(
        timeline
            .change_character_knowledge(selected, CharacterId::Crazy, EventId::new(), true, vec![])
            .is_err()
    );
    assert_eq!(timeline.store.world(timeline.branch).unwrap(), original);
}

#[test]
fn hiding_and_forgetting_are_distinct() {
    let mut timeline = SessionTimeline::new();
    let e = message(&mut timeline, CharacterId::Crazy, "hidden");
    timeline
        .change_message(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            message_actions::MessageAction::Hide { targets: vec![e] },
        )
        .unwrap();
    assert_eq!(
        message_ids(&timeline, CharacterId::Crazy, 2),
        Vec::<EventId>::new()
    );
    assert!(
        timeline
            .store
            .knows(timeline.branch, CharacterId::Crazy, e, None)
            .unwrap()
    );
    timeline
        .change_character_knowledge(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            false,
            vec![e],
        )
        .unwrap();
    timeline
        .change_character_knowledge(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            true,
            vec![e],
        )
        .unwrap();
    assert!(message_ids(&timeline, CharacterId::Crazy, 4).is_empty());
    timeline
        .change_message(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            message_actions::MessageAction::Restore { target: e },
        )
        .unwrap();
    assert_eq!(message_ids(&timeline, CharacterId::Crazy, 5), vec![e]);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_reopen_preserves_epoch_and_revisioned_knowledge() {
    let db = std::env::temp_dir().join(format!("nm-knowledge-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    {
        let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root,
        })
        .unwrap();
        let e = message(&mut timeline, CharacterId::Crazy, "first");
        let private = message(&mut timeline, CharacterId::Kind, "other");
        timeline
            .change_character_knowledge(
                timeline.selection(),
                CharacterId::Crazy,
                EventId::new(),
                false,
                vec![e],
            )
            .unwrap();
        assert!(
            !timeline
                .store
                .knows(root, CharacterId::Crazy, e, None)
                .unwrap()
        );
        timeline
            .change_character_knowledge(
                timeline.selection(),
                CharacterId::Crazy,
                EventId::new(),
                true,
                vec![e, private],
            )
            .unwrap();
        assert_eq!(
            timeline.store.knows(root, CharacterId::Crazy, e, None),
            Ok(true)
        );
        assert_eq!(
            timeline.store.knows(root, CharacterId::Crazy, e, Some(3)),
            Ok(false)
        );
        assert_eq!(
            timeline.store.context_epoch(root, CharacterId::Crazy),
            Ok(4)
        );
    }
    {
        let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root,
        })
        .unwrap();
        assert_eq!(
            timeline.store.context_epoch(root, CharacterId::Crazy),
            Ok(4)
        );
        let current = view(&timeline, CharacterId::Crazy, 4);
        assert_eq!(
            current
                .iter()
                .filter(|known| known.learned_at == 4
                    && known.fact.draft.kind == EventKind::PlayerMessage)
                .count(),
            2
        );
        assert_eq!(message_ids(&timeline, CharacterId::Crazy, 1).len(), 1);
        assert_eq!(message_ids(&timeline, CharacterId::Crazy, 3).len(), 0);
        assert_eq!(message_ids(&timeline, CharacterId::Crazy, 4).len(), 2);
        assert_eq!(message_ids(&timeline, CharacterId::Kind, 4).len(), 1);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

fn accepted(
    timeline: &mut SessionTimeline,
    character: CharacterId,
    content: &str,
) -> RegenerateSource {
    let original_input = message(timeline, character, content);
    let accepted_request = RequestId::new();
    let mut reply = ConversationMessage::new(
        EventId::new().0.to_string(),
        ConversationSpeaker::Character(character),
        json!("response"),
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
fn same_branch_regenerate_plan_targets_only_authored_future_replies() {
    let mut timeline = SessionTimeline::new();
    let first = accepted(&mut timeline, CharacterId::Crazy, "initial");
    let kind = accepted(
        &mut timeline,
        CharacterId::Kind,
        "separate private conversation",
    );
    let later = accepted(
        &mut timeline,
        CharacterId::Crazy,
        "later Crazy conversation",
    );
    let selected = timeline.selection();
    let before = timeline.store.world(timeline.branch).unwrap();
    let plan = timeline
        .plan_knowledge_regenerate(first, selected, 6)
        .unwrap();
    assert_eq!(plan.selection, selected);
    assert_eq!(plan.head, 6);
    assert_eq!(plan.source, first);
    assert_eq!(
        plan.hide_messages,
        vec![first.accepted_message, later.accepted_message]
    );
    assert_eq!(plan.forget_events, plan.hide_messages);
    assert!(plan.related_effects.is_empty());
    assert!(!plan.hide_messages.contains(&kind.accepted_message));
    assert!(!plan.forget_events.contains(&first.original_input));
    assert_eq!(timeline.store.world(timeline.branch).unwrap(), before);
    assert_eq!(timeline.selection(), selected);
    assert_eq!(
        timeline
            .store
            .branch(timeline.branch)
            .unwrap()
            .head_sequence,
        6
    );
}

#[test]
fn same_branch_regenerate_plan_rejects_forged_and_stale_without_mutation() {
    let mut timeline = SessionTimeline::new();
    let source = accepted(&mut timeline, CharacterId::Crazy, "source");
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
        assert!(
            timeline
                .plan_knowledge_regenerate(forged, selected, 2)
                .is_err()
        );
    }
    assert!(
        timeline
            .plan_knowledge_regenerate(source, selected, 1)
            .is_err()
    );
    assert_eq!(timeline.store.world(timeline.branch).unwrap(), before);
}

#[test]
fn revised_message_remains_revised_after_forget_and_reveal() {
    let mut timeline = SessionTimeline::new();
    let e = message(&mut timeline, CharacterId::Crazy, "original");
    let mut changes = serde_json::Map::new();
    changes.insert("content".into(), json!("revised"));
    timeline
        .change_message(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            message_actions::MessageAction::Revise { target: e, changes },
        )
        .unwrap();
    timeline
        .change_character_knowledge(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            false,
            vec![e],
        )
        .unwrap();
    timeline
        .change_character_knowledge(
            timeline.selection(),
            CharacterId::Crazy,
            EventId::new(),
            true,
            vec![e],
        )
        .unwrap();
    let current = TimelineMessageQueries::capture(
        timeline.store.as_ref(),
        timeline.branch,
        CharacterId::Crazy,
        None,
    )
    .unwrap();
    assert_eq!(current.messages()[0].content, "revised");
}

#[test]
fn same_branch_controls_are_atomic_and_preserve_other_mitas() {
    let mut timeline = SessionTimeline::new();
    let initial = accepted(&mut timeline, CharacterId::Crazy, "original");
    let other = accepted(&mut timeline, CharacterId::Kind, "other character");
    let later = accepted(&mut timeline, CharacterId::Crazy, "later");
    let before = timeline.selection();
    let head = timeline.anchor().through;
    let proposal = timeline
        .plan_knowledge_regenerate(initial, before, head)
        .unwrap();
    let old = timeline.store.world(timeline.branch).unwrap();
    let mut tampered = proposal.clone();
    tampered.forget_events.clear();
    assert!(
        timeline
            .commit_regenerate_knowledge_controls(&tampered, EventId::new(), EventId::new())
            .is_err()
    );
    assert_eq!(timeline.store.world(timeline.branch).unwrap(), old);

    let after = timeline
        .commit_regenerate_knowledge_controls(&proposal, EventId::new(), EventId::new())
        .unwrap();
    assert_eq!(timeline.selection(), before);
    assert_eq!(after.through, head + 2);
    assert_eq!(timeline.branch.0, after.branch_id);
    let crazy = message_ids(&timeline, CharacterId::Crazy, after.through);
    assert!(!crazy.contains(&initial.accepted_message));
    assert!(!crazy.contains(&later.accepted_message));
    assert!(crazy.contains(&initial.original_input));
    let kind = message_ids(&timeline, CharacterId::Kind, after.through);
    assert!(kind.contains(&other.accepted_message));
    assert!(kind.contains(&other.original_input));
    assert!(
        !timeline
            .store
            .knows(
                timeline.branch,
                CharacterId::Crazy,
                initial.accepted_message,
                None
            )
            .unwrap()
    );
    assert!(
        timeline
            .store
            .knows(
                timeline.branch,
                CharacterId::Kind,
                other.accepted_message,
                None
            )
            .unwrap()
    );
    assert_eq!(
        message_ids(&timeline, CharacterId::Crazy, head).contains(&initial.accepted_message),
        true
    );
    assert_eq!(
        timeline.store.world(timeline.branch).unwrap().len(),
        old.len() + 2
    );
    assert!(
        timeline
            .commit_regenerate_knowledge_controls(&proposal, EventId::new(), EventId::new())
            .is_err()
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_same_branch_controls_survive_reopen_and_reject_stale_writer() {
    let db = std::env::temp_dir().join(format!("nm-regenerate-controls-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut first = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let source = accepted(&mut first, CharacterId::Crazy, "input");
    let other = accepted(&mut first, CharacterId::Kind, "untouched");
    let mut second = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let plan = first
        .plan_knowledge_regenerate(source, first.selection(), first.anchor().through)
        .unwrap();
    let stale = second
        .plan_knowledge_regenerate(source, second.selection(), second.anchor().through)
        .unwrap();
    let committed = first
        .commit_regenerate_knowledge_controls(&plan, EventId::new(), EventId::new())
        .unwrap();
    assert_eq!(
        second.commit_regenerate_knowledge_controls(&stale, EventId::new(), EventId::new()),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    drop((first, second));
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.selection().branch_id, root);
    assert_eq!(reopened.anchor().through, committed.through);
    assert!(
        !reopened
            .store
            .knows(root, CharacterId::Crazy, source.accepted_message, None)
            .unwrap()
    );
    assert!(
        reopened
            .store
            .knows(root, CharacterId::Kind, other.accepted_message, None)
            .unwrap()
    );
    assert!(
        reopened
            .store
            .knows(
                root,
                CharacterId::Crazy,
                source.accepted_message,
                Some(plan.head)
            )
            .unwrap()
    );
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_control_batch_rollback_does_not_hide_reply_when_forget_insert_fails() {
    let db = std::env::temp_dir().join(format!("nm-regenerate-rollback-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let source = accepted(&mut timeline, CharacterId::Crazy, "original");
    let selection = timeline.selection();
    let plan = timeline
        .plan_knowledge_regenerate(source, selection, timeline.anchor().through)
        .unwrap();
    let head = plan.head;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_regenerate_forget BEFORE INSERT ON timeline_events WHEN json_extract(NEW.fact_json,'$.draft.kind')='CharacterKnowledgeForgotten' BEGIN SELECT RAISE(ABORT, 'injected forget failure'); END;").unwrap();
    assert!(
        timeline
            .commit_regenerate_knowledge_controls(&plan, EventId::new(), EventId::new())
            .is_err()
    );
    drop(timeline);
    let mut reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(reopened.anchor().through, head);
    assert!(message_ids(&reopened, CharacterId::Crazy, head).contains(&source.accepted_message));
    assert!(
        reopened
            .store
            .knows(root, CharacterId::Crazy, source.accepted_message, None)
            .unwrap()
    );
    conn.execute_batch("DROP TRIGGER reject_regenerate_forget;")
        .unwrap();
    let committed = reopened
        .commit_regenerate_knowledge_controls(&plan, EventId::new(), EventId::new())
        .unwrap();
    assert_eq!(committed.through, head + 2);
    drop((conn, reopened));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}
