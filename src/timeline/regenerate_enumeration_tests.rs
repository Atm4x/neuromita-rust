use super::*;

#[test]
fn transient_enumeration_is_character_scoped_read_only_and_classifies_lifecycle() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let original_selection = timeline.selection();
    let original_world = timeline.store.world(child).unwrap();
    let snapshots = timeline
        .enumerate_regenerate_admissions(source.character)
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].admission.request, request);
    assert_eq!(
        snapshots[0].outcome,
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    assert_eq!(
        timeline
            .enumerate_regenerate_admissions(CharacterId::Kind)
            .unwrap(),
        vec![]
    );
    assert_eq!(timeline.store.world(child).unwrap(), original_world);
    assert_eq!(timeline.selection(), original_selection);

    let dispatch = timeline
        .permit_regenerate_dispatch(child, request, original_selection)
        .unwrap();
    assert_eq!(
        timeline
            .enumerate_regenerate_admissions(source.character)
            .unwrap()[0]
            .outcome,
        RegenerateRecoveryOutcome::MayHaveDispatched(dispatch)
    );
    let abandoned = timeline
        .abandon_regenerate(child, request, original_selection)
        .unwrap();
    assert_eq!(
        timeline
            .enumerate_regenerate_admissions(source.character)
            .unwrap()[0]
            .outcome,
        RegenerateRecoveryOutcome::Abandoned(abandoned)
    );
}

#[test]
fn transient_enumeration_includes_historical_children_without_replay() {
    let mut timeline = SessionTimeline::new();
    let parent = timeline.selection().branch_id;
    let (source, first, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let back = timeline
        .select_branch(parent, timeline.selection())
        .unwrap();
    let second = BranchId::new();
    let second_request = RequestId::new();
    let receipt = RegenerateAdmissionReceipt {
        request: second_request,
        accepted_request: source.accepted_request,
        accepted_message: source.accepted_message,
        original_input: source.original_input,
        character: source.character,
        parent_branch: parent,
        parent_head: 2,
        parent_generation: back.generation,
        child_branch: second,
        configuration: RegenerateConfigurationReference {
            settings_revision: 7,
            preset_id: 2,
            provider_model: "recovery-model".into(),
            prompt_set: None,
        },
    };
    timeline
        .fork_and_admit_regenerate_source(source, second, back, 2, &receipt)
        .unwrap();
    let prior = timeline.store.world(first).unwrap();
    let enumerated = timeline
        .enumerate_regenerate_admissions(source.character)
        .unwrap();
    assert_eq!(enumerated.len(), 2);
    assert!(
        enumerated
            .iter()
            .any(|entry| entry.admission.request == request)
    );
    assert!(
        enumerated
            .iter()
            .any(|entry| entry.admission.request == second_request)
    );
    assert!(
        enumerated
            .iter()
            .all(|entry| entry.outcome == RegenerateRecoveryOutcome::DispatchUnknown)
    );
    assert_eq!(timeline.store.world(first).unwrap(), prior);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_enumeration_survives_reopen_and_aba_without_other_character_disclosure() {
    let path = std::env::temp_dir().join(format!("b70-enumerate-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let selection = timeline.selection();
    let world = timeline.store.world(child).unwrap();
    drop(timeline);
    let mut reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let observed = reopened
        .enumerate_regenerate_admissions(source.character)
        .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].admission.request, request);
    assert_eq!(observed[0].selection, selection);
    assert_eq!(
        reopened
            .enumerate_regenerate_admissions(CharacterId::Kind)
            .unwrap(),
        vec![]
    );
    let back = reopened.select_branch(root, selection).unwrap();
    let returned = reopened.select_branch(child, back).unwrap();
    assert!(returned.generation > selection.generation);
    let refreshed = reopened
        .enumerate_regenerate_admissions(source.character)
        .unwrap();
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].selection, returned);
    assert_eq!(
        refreshed[0].outcome,
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    assert_eq!(reopened.store.world(child).unwrap(), world);
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn archive_roundtrip_preserves_indexable_admissions_and_rejects_forged_recipient() {
    use crate::timeline::storage::{SqliteTimelineRepository, archive, assets::TimelineAssetStore};
    let path = std::env::temp_dir().join(format!("b70-archive-{}.db", uuid::Uuid::new_v4()));
    let directory = std::env::temp_dir().join(format!("b70-asset-{}", uuid::Uuid::new_v4()));
    let assets = TimelineAssetStore::new(&directory, 1024 * 1024).unwrap();
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    drop(timeline);
    let repository = SqliteTimelineRepository::open(&path).unwrap();
    let bytes = archive::export_sqlite(&repository, session, &assets).unwrap();
    let mut imported = TimelineStore::new();
    archive::import_memory(&mut imported, &bytes, &assets).unwrap();
    assert_eq!(
        imported
            .regenerate_admission_branches(session, source.character)
            .unwrap(),
        vec![child]
    );
    assert_eq!(
        imported
            .regenerate_admission_branches(session, CharacterId::Kind)
            .unwrap(),
        vec![]
    );
    assert_eq!(
        imported
            .regenerate_admission_branches(SessionId::new(), source.character)
            .unwrap(),
        vec![]
    );
    let marker = imported
        .world(child)
        .unwrap()
        .into_iter()
        .find(|fact| fact.branch_id == child && fact.draft.kind == EventKind::BranchCreated)
        .unwrap();
    assert_eq!(
        RegenerateAdmissionReceipt::from_marker(&marker.draft.payload)
            .unwrap()
            .unwrap()
            .request,
        request
    );

    let mut forged: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let branch = forged["branches"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["info"]["id"] == serde_json::json!(child))
        .unwrap();
    branch["events"][0]["draft"]["payload"][RegenerateAdmissionReceipt::KEY]["character"] =
        serde_json::json!(CharacterId::Kind);
    let mut rejected = TimelineStore::new();
    assert!(
        archive::import_memory(
            &mut rejected,
            &serde_json::to_vec(&forged).unwrap(),
            &assets
        )
        .is_err()
    );
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
    let _ = std::fs::remove_dir_all(directory);
}

#[test]
fn enumeration_classifies_accepted_reply_without_resubmitting_or_reapplying_effects() {
    use serde_json::json;
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
    let accepted = timeline
        .append_reply(GeneratedReply {
            request,
            intent: TurnIntent::Reply(source.original_input),
            turn: pinned,
            expected_revision: 0,
            content: json!("regenerated"),
            audience: ConversationAudience::private_to(source.character),
            effects: Vec::new(),
        })
        .unwrap();
    let before = timeline.store.world(child).unwrap();
    let result = timeline
        .enumerate_regenerate_admissions(source.character)
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result[0].outcome,
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    assert_eq!(timeline.store.world(child).unwrap(), before);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_enumeration_revision_detects_independent_writer_changes() {
    use crate::timeline::selection::TimelineSelectionRepository;
    use crate::timeline::storage::SqliteTimelineRepository;
    let path = std::env::temp_dir().join(format!("b70-revision-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let first = timeline.store.enumeration_revision().unwrap();
    let mut other_owner = SqliteTimelineRepository::open(&path).unwrap();
    let selection = other_owner.selection().unwrap().unwrap();
    let changed_selection = other_owner.select_branch(root, Some(selection)).unwrap();
    assert_ne!(changed_selection, selection);
    assert!(changed_selection.generation > selection.generation);
    // Selection writes need not advance the timeline fact revision.
    assert!(timeline.store.enumeration_revision().unwrap() >= first);
    assert_ne!(other_owner.selection().unwrap(), Some(selection));
    drop((timeline, other_owner));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_recovery_uses_request_index_without_scanning_unrelated_worlds() {
    use crate::timeline::storage::SqliteTimelineRepository;
    use serde_json::json;

    let path = std::env::temp_dir().join(format!("b71-index-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let repository = SqliteTimelineRepository::open(&path).unwrap();
    assert!(
        repository
            .regenerate_request_facts(child, request.0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        timeline
            .reconcile_regenerate(child, request)
            .unwrap()
            .outcome,
        RegenerateRecoveryOutcome::DispatchUnknown
    );
    let pinned = timeline
        .capture_turn(
            source.character,
            Some(source.original_input),
            timeline.anchor(),
        )
        .unwrap();
    let dispatch = timeline
        .permit_regenerate_dispatch(child, request, timeline.selection())
        .unwrap();
    let accepted = timeline
        .append_reply(GeneratedReply {
            request,
            intent: TurnIntent::Reply(source.original_input),
            turn: pinned,
            expected_revision: 0,
            content: json!("recovered from indexed child"),
            audience: ConversationAudience::private_to(source.character),
            effects: Vec::new(),
        })
        .unwrap();
    let indexed = repository
        .regenerate_request_facts(child, request.0)
        .unwrap();
    assert_eq!(indexed.len(), 2);
    assert_eq!(indexed[0].draft.id, dispatch);
    assert_eq!(indexed[1].draft.id, accepted);
    assert!(indexed.iter().all(|fact| fact.branch_id == child));
    assert!(
        repository
            .regenerate_request_facts(root, request.0)
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .regenerate_request_facts(child, RequestId::new().0)
            .unwrap()
            .is_empty()
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    let mut statement = connection
        .prepare("EXPLAIN QUERY PLAN SELECT fact_json FROM timeline_events WHERE branch_id=? AND json_extract(fact_json,'$.draft.payload.request_id')=? AND json_extract(fact_json,'$.draft.payload.request_id') IS NOT NULL ORDER BY sequence")
        .unwrap();
    let plan = statement
        .query_map([child.0.to_string(), request.0.to_string()], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert!(
        plan.iter()
            .any(|step| step.contains("timeline_regenerate_events_by_request")),
        "indexed query plan: {plan:?}"
    );
    drop(statement);
    drop((connection, repository, timeline));
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(
        reopened
            .enumerate_regenerate_admissions(source.character)
            .unwrap()[0]
            .outcome,
        RegenerateRecoveryOutcome::AcceptedReply(accepted)
    );
    assert!(
        reopened
            .enumerate_regenerate_admissions(CharacterId::Kind)
            .unwrap()
            .is_empty()
    );
    drop(reopened);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[test]
fn transient_indexed_lifecycle_contract_is_branch_local() {
    let mut timeline = SessionTimeline::new();
    let (source, child, request) =
        super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let root = timeline.store.branch(child).unwrap().parent.unwrap();
    let dispatch = timeline
        .permit_regenerate_dispatch(child, request, timeline.selection())
        .unwrap();
    let indexed = timeline
        .store
        .regenerate_request_facts(child, request.0)
        .unwrap();
    assert_eq!(indexed.len(), 1);
    assert_eq!(indexed[0].draft.id, dispatch);
    assert_eq!(indexed[0].branch_id, child);
    assert!(
        timeline
            .store
            .regenerate_request_facts(root, request.0)
            .unwrap()
            .is_empty()
    );
    assert!(
        timeline
            .enumerate_regenerate_admissions(CharacterId::Kind)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        timeline
            .enumerate_regenerate_admissions(source.character)
            .unwrap()[0]
            .outcome,
        RegenerateRecoveryOutcome::MayHaveDispatched(dispatch)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_indexed_accepted_source_obeys_parent_cutoff_and_character_scope() {
    use crate::timeline::storage::SqliteTimelineRepository;

    let path = std::env::temp_dir().join(format!("b71-ancestor-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let (source, child, _) = super::regenerate_owner_tests::admitted_recovery_source(&mut timeline);
    let repo = SqliteTimelineRepository::open(&path).unwrap();
    assert_eq!(
        repo.accepted_reply_at(root, source.character, source.accepted_request.0, Some(1))
            .unwrap(),
        None
    );
    let root_accepted = repo
        .accepted_reply_at(root, source.character, source.accepted_request.0, Some(2))
        .unwrap()
        .unwrap();
    assert_eq!(root_accepted.draft.id, source.accepted_message);
    assert_eq!(
        repo.accepted_reply_at(child, source.character, source.accepted_request.0, Some(1))
            .unwrap(),
        None
    );
    assert_eq!(
        repo.accepted_reply_at(child, source.character, source.accepted_request.0, None)
            .unwrap(),
        None
    );
    assert_eq!(
        repo.accepted_reply_at(child, CharacterId::Kind, source.accepted_request.0, None)
            .unwrap(),
        None
    );
    assert!(
        repo.accepted_reply_at(
            child,
            source.character,
            source.accepted_request.0,
            Some(100)
        )
        .is_err()
    );
    drop((repo, timeline));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

fn b72_many_admissions(
    timeline: &mut SessionTimeline,
    count: usize,
) -> (SessionId, BranchId, CharacterId) {
    let root = timeline.selection().branch_id;
    let session = timeline.store.branch(root).unwrap().session_id;
    let (source, _, _) = super::regenerate_owner_tests::admitted_recovery_source(timeline);
    for _ in 1..count {
        let back = timeline.select_branch(root, timeline.selection()).unwrap();
        let child = BranchId::new();
        let receipt = RegenerateAdmissionReceipt {
            request: RequestId::new(),
            accepted_request: source.accepted_request,
            accepted_message: source.accepted_message,
            original_input: source.original_input,
            character: source.character,
            parent_branch: root,
            parent_head: 2,
            parent_generation: back.generation,
            child_branch: child,
            configuration: RegenerateConfigurationReference {
                settings_revision: 7,
                preset_id: 2,
                provider_model: "recovery-model".into(),
                prompt_set: None,
            },
        };
        timeline
            .fork_and_admit_regenerate_source(source, child, back, 2, &receipt)
            .unwrap();
    }
    (session, root, source.character)
}

fn b72_collect_pages(
    timeline: &SessionTimeline,
    character: CharacterId,
    limit: usize,
) -> Vec<RegenerateRecoverySnapshot> {
    let mut output = Vec::new();
    let mut cursor = None;
    loop {
        let page = timeline
            .enumerate_regenerate_admissions_page(character, limit, cursor)
            .unwrap();
        assert!(page.entries.len() <= limit);
        output.extend(page.entries);
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    output
}

#[test]
fn keyset_pages_sizes_one_two_empty_and_character_scope() {
    let mut timeline = SessionTimeline::new();
    let (_session, _root, character) = b72_many_admissions(&mut timeline, 9);
    let original = timeline.enumerate_regenerate_admissions(character).unwrap();
    assert_eq!(original.len(), 9);
    for limit in [1, 2, 3, 8, 128] {
        let pages = b72_collect_pages(&timeline, character, limit);
        assert_eq!(pages, original);
        assert_eq!(pages.len(), 9);
    }
    let empty = timeline
        .enumerate_regenerate_admissions_page(CharacterId::Kind, 1, None)
        .unwrap();
    assert!(empty.entries.is_empty());
    assert!(empty.next.is_none());
    assert!(matches!(
        timeline.enumerate_regenerate_admissions_page(character, 0, None),
        Err(domain::TimelineError::Invalid(_))
    ));
    assert!(matches!(
        timeline.enumerate_regenerate_admissions_page(character, 129, None),
        Err(domain::TimelineError::Invalid(_))
    ));
    let first = timeline
        .enumerate_regenerate_admissions_page(character, 1, None)
        .unwrap();
    assert_eq!(
        timeline.enumerate_regenerate_admissions_page(CharacterId::Kind, 1, first.next),
        Err(domain::TimelineError::Conflict)
    );
}

#[test]
fn transient_keyset_cursor_rejects_selection_aba() {
    let mut timeline = SessionTimeline::new();
    let (_session, root, character) = b72_many_admissions(&mut timeline, 3);
    let first = timeline
        .enumerate_regenerate_admissions_page(character, 1, None)
        .unwrap();
    let original = timeline.selection();
    let back = timeline.select_branch(root, original).unwrap();
    let returned = timeline.select_branch(original.branch_id, back).unwrap();
    assert!(returned.generation > original.generation);
    assert_eq!(
        timeline.enumerate_regenerate_admissions_page(character, 1, first.next),
        Err(domain::TimelineError::ConcurrentWrite)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_keyset_reopen_other_writer_append_and_aba_invalidate_cursor() {
    let path = std::env::temp_dir().join(format!("b72-pages-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    let (_, _, character) = b72_many_admissions(&mut timeline, 7);
    let expected = b72_collect_pages(&timeline, character, 2);
    assert_eq!(
        expected,
        timeline.enumerate_regenerate_admissions(character).unwrap()
    );
    let first = timeline
        .enumerate_regenerate_admissions_page(character, 2, None)
        .unwrap();
    drop(timeline);
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(b72_collect_pages(&reopened, character, 1), expected);
    let second = reopened
        .enumerate_regenerate_admissions_page(character, 2, first.next)
        .unwrap();
    assert_eq!(second.entries, expected[2..4]);
    let mut writer = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    writer
        .publish_player_message(
            character,
            EventId::new(),
            std::sync::Arc::from("concurrent player"),
        )
        .unwrap();
    assert_eq!(
        reopened.enumerate_regenerate_admissions_page(character, 2, first.next),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    let after_append = reopened
        .enumerate_regenerate_admissions_page(character, 1, None)
        .unwrap();
    let previous = writer.selection();
    let back = writer.select_branch(root, previous).unwrap();
    writer.select_branch(previous.branch_id, back).unwrap();
    assert_eq!(
        reopened.enumerate_regenerate_admissions_page(character, 1, after_append.next),
        Err(domain::TimelineError::ConcurrentWrite)
    );
    drop((reopened, writer));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_keyset_rejects_same_request_on_another_page() {
    let path = std::env::temp_dir().join(format!("b72-duplicate-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    b72_many_admissions(&mut timeline, 3);
    let pages = timeline
        .enumerate_regenerate_admissions(CharacterId::Crazy)
        .unwrap();
    let first = &pages[0].admission;
    let other = &pages[2].admission;
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute(
        "UPDATE timeline_events SET fact_json=json_set(fact_json,'$.draft.payload.regenerate_admission.request',?) WHERE branch_id=? AND sequence=(SELECT fork_sequence+1 FROM timeline_branches WHERE branch_id=?)",
        rusqlite::params![first.request.0.to_string(), other.child_branch.0.to_string(), other.child_branch.0.to_string()],
    ).unwrap();
    assert_eq!(
        timeline.enumerate_regenerate_admissions_page(CharacterId::Crazy, 1, None),
        Err(domain::TimelineError::Conflict)
    );
    drop((timeline, connection));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_keyset_uses_recipient_index_and_bounded_source_rows() {
    let path = std::env::temp_dir().join(format!("b72-index-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    b72_many_admissions(&mut timeline, 22);
    let result = timeline
        .enumerate_regenerate_admissions_page(CharacterId::Crazy, 2, None)
        .unwrap();
    assert_eq!(result.entries.len(), 2);
    assert!(result.next.is_some());
    let connection = rusqlite::Connection::open(&path).unwrap();
    let mut statement = connection.prepare("EXPLAIN QUERY PLAN SELECT e.branch_id,e.fact_json FROM timeline_events e INDEXED BY timeline_regenerate_markers_by_character JOIN timeline_branches b ON b.branch_id=e.branch_id WHERE json_extract(e.fact_json,'$.draft.payload.regenerate_admission.character')=? AND json_extract(e.fact_json,'$.draft.payload.regenerate_admission.character') IS NOT NULL AND e.branch_id>? AND b.session_id=? AND b.parent_id IS NOT NULL AND e.sequence=b.fork_sequence+1 ORDER BY e.branch_id LIMIT ?").unwrap();
    let plans: Vec<String> = statement
        .query_map(
            rusqlite::params!["Crazy", "", session.0.to_string(), "3"],
            |row| row.get(3),
        )
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plans
            .iter()
            .any(|plan| plan.contains("timeline_regenerate_markers_by_character")),
        "{plans:?}"
    );
    drop(statement);
    assert_eq!(
        b72_collect_pages(&timeline, CharacterId::Crazy, 2).len(),
        22
    );
    drop((timeline, connection));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}

#[test]
fn keyset_cursor_is_bound_to_originating_session() {
    let mut first = SessionTimeline::new();
    b72_many_admissions(&mut first, 3);
    let page = first
        .enumerate_regenerate_admissions_page(CharacterId::Crazy, 1, None)
        .unwrap();
    assert!(page.next.is_some());
    let mut another = SessionTimeline::new();
    b72_many_admissions(&mut another, 3);
    assert_eq!(
        another.enumerate_regenerate_admissions_page(CharacterId::Crazy, 1, page.next),
        Err(domain::TimelineError::Conflict)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn archive_import_preserves_keyset_source_order_and_character_scope() {
    use crate::timeline::storage::{SqliteTimelineRepository, archive, assets::TimelineAssetStore};
    let path = std::env::temp_dir().join(format!("b72-archive-{}.db", uuid::Uuid::new_v4()));
    let asset_path = std::env::temp_dir().join(format!("b72-assets-{}", uuid::Uuid::new_v4()));
    let assets = TimelineAssetStore::new(&asset_path, 1024 * 1024).unwrap();
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    b72_many_admissions(&mut timeline, 4);
    let expected = timeline
        .enumerate_regenerate_admissions(CharacterId::Crazy)
        .unwrap()
        .into_iter()
        .map(|entry| entry.admission.child_branch)
        .collect::<Vec<_>>();
    drop(timeline);
    let repository = SqliteTimelineRepository::open(&path).unwrap();
    let archive_bytes = archive::export_sqlite(&repository, session, &assets).unwrap();
    let mut restored = TimelineStore::new();
    archive::import_memory(&mut restored, &archive_bytes, &assets).unwrap();
    let mut after = None;
    let mut seen = Vec::new();
    loop {
        let page = restored
            .regenerate_admission_branches_page(session, CharacterId::Crazy, after, 2)
            .unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().copied();
        seen.extend(page);
    }
    assert_eq!(seen, expected);
    assert!(
        restored
            .regenerate_admission_branches_page(session, CharacterId::Kind, None, 2)
            .unwrap()
            .is_empty()
    );
    assert!(
        restored
            .regenerate_admission_branches_page(SessionId::new(), CharacterId::Crazy, None, 2)
            .unwrap()
            .is_empty()
    );
    drop((repository, restored));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
    let _ = std::fs::remove_dir_all(asset_path);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_keyset_scans_admission_markers_not_unrelated_player_history() {
    let path = std::env::temp_dir().join(format!("b72-history-{}.db", uuid::Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let mut timeline = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    })
    .unwrap();
    b72_many_admissions(&mut timeline, 10);
    for _ in 0..120 {
        timeline
            .publish_player_message(
                CharacterId::Crazy,
                EventId::new(),
                std::sync::Arc::from("unrelated input"),
            )
            .unwrap();
    }
    let original_selection = timeline.selection();
    let original_revision = timeline.store.enumeration_revision().unwrap();
    let pages = b72_collect_pages(&timeline, CharacterId::Crazy, 1);
    assert_eq!(pages.len(), 10);
    assert_eq!(timeline.selection(), original_selection);
    assert_eq!(
        timeline.store.enumeration_revision().unwrap(),
        original_revision
    );
    drop(timeline);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
}
