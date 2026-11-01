use neuromita::timeline::view::TimelineMessageQueries;
use neuromita::{
    character::CharacterId,
    timeline::domain::{
        BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId, TimelineError,
        TimelineRepository, TimelineStore,
    },
};
use serde_json::json;

fn push(repo: &mut dyn TimelineRepository, branch: BranchId, drafts: Vec<EventDraft>) {
    let head = repo.branch(branch).unwrap().head_sequence;
    let key = format!("mitaview:{head}");
    repo.append(branch, head, &key, json!({}), drafts).unwrap();
}

fn player(text: &str, observers: &[CharacterId]) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft
        .payload
        .insert("content".into(), json!([{"type": "text", "text": text}]));
    draft.payload.insert(
        "message".into(),
        json!({"metadata": {"origin": "unity"}, "time": "2026-11-01 10:00:00"}),
    );
    draft.observers.extend(observers);
    draft
}

fn mutation(kind: EventKind, who: CharacterId, key: &str, value: serde_json::Value) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.observers.insert(who);
    draft.payload.insert(key.into(), value);
    draft
}

fn run_projection_contract(repo: &mut dyn TimelineRepository) {
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let secret = player("private", &[CharacterId::Mita]);
    let secret_id = secret.id;
    push(repo, branch, vec![secret]);
    let first = TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, None).unwrap();
    assert_eq!(first.through, 1);
    assert_eq!(first.count(), 1);
    assert_eq!(
        first.provider_messages()[0]["content"],
        json!([{"type": "text", "text": "private"}])
    );
    assert_eq!(first.provider_messages()[0]["metadata"]["origin"], "unity");
    assert_eq!(first.provider_messages()[0]["timeline_learned_at"], 1);
    assert_eq!(first.provider_messages()[0]["time"], "2026-11-01 10:00:00");
    assert!(first.contains(&secret_id.0.to_string()));
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::KindMita, None)
            .unwrap()
            .count(),
        0
    );

    let mut disclosure = EventDraft::new(EventKind::EventObserved);
    disclosure.observers.insert(CharacterId::KindMita);
    disclosure.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: secret_id,
    });
    push(repo, branch, vec![disclosure]);
    let learned =
        TimelineMessageQueries::capture(repo, branch, CharacterId::KindMita, None).unwrap();
    assert_eq!(learned.count(), 1);
    assert_eq!(learned.provider_messages()[0]["timeline_sequence"], 1);
    assert_eq!(learned.provider_messages()[0]["timeline_learned_at"], 2);

    let mut revise = mutation(
        EventKind::MessageRevised,
        CharacterId::Mita,
        "event_id",
        json!(secret_id.0.to_string()),
    );
    revise.payload.insert(
        "message".into(),
        json!({"content": "edited", "annotation": "after revision"}),
    );
    push(repo, branch, vec![revise]);
    let edited = TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, None).unwrap();
    assert_eq!(edited.count(), 1);
    assert_eq!(edited.messages()[0].content, "edited");
    assert_eq!(
        edited.provider_messages()[0]["annotation"],
        "after revision"
    );
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::KindMita, None)
            .unwrap()
            .provider_messages()[0]["content"],
        json!([{"type":"text","text":"private"}])
    );

    push(
        repo,
        branch,
        vec![mutation(
            EventKind::MessageHidden,
            CharacterId::Mita,
            "event_ids",
            json!([secret_id.0.to_string()]),
        )],
    );
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, None)
            .unwrap()
            .count(),
        0
    );
    push(
        repo,
        branch,
        vec![mutation(
            EventKind::MessageRestored,
            CharacterId::Mita,
            "event_id",
            json!(secret_id.0.to_string()),
        )],
    );
    let restored = TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, None).unwrap();
    assert_eq!(restored.count(), 1);
    assert_eq!(restored.messages()[0].content, "edited");

    let reset = mutation(
        EventKind::CharacterContextReset,
        CharacterId::Mita,
        "reason",
        json!("test"),
    );
    push(repo, branch, vec![reset]);
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, None)
            .unwrap()
            .count(),
        0
    );
    assert_eq!(first.count(), 1);
    let mut disclose_again = EventDraft::new(EventKind::EventObserved);
    disclose_again.observers.insert(CharacterId::Mita);
    disclose_again.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: secret_id,
    });
    push(repo, branch, vec![disclose_again]);
    let relearned = TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, None).unwrap();
    assert_eq!(relearned.count(), 1);
    assert_eq!(relearned.messages()[0].learned_at, 7);
    assert_eq!(relearned.messages()[0].content, "");
    assert_eq!(
        relearned.provider_messages()[0]["content"],
        json!([{"type":"text","text":"private"}])
    );
    let before_reset =
        TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, Some(5)).unwrap();
    assert_eq!(before_reset.messages()[0].content, "edited");
    assert_eq!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, Some(6))
            .unwrap()
            .count(),
        0
    );
    assert!(matches!(
        TimelineMessageQueries::capture(repo, branch, CharacterId::Mita, Some(50)),
        Err(TimelineError::Invalid(_))
    ));
}

#[test]
fn projection_preserves_private_knowledge_and_snapshot_semantics_in_memory() {
    let mut repository = TimelineStore::new();
    run_projection_contract(&mut repository);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn projection_preserves_private_knowledge_and_snapshot_semantics_in_sqlite() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path = std::env::temp_dir().join(format!("mitaview-{}.sqlite", uuid::Uuid::new_v4()));
    {
        let mut repository = SqliteTimelineRepository::open(&path).unwrap();
        run_projection_contract(&mut repository);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn run_paging_contract(repository: &mut dyn TimelineRepository) {
    let root = BranchId::new();
    repository.create_session(SessionId::new(), root).unwrap();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    push(repository, root, vec![checkpoint]);
    for index in 0..142 {
        push(
            repository,
            root,
            vec![player(&format!("message {index}"), &[CharacterId::Mita])],
        );
    }
    let snapshot =
        TimelineMessageQueries::capture(repository, root, CharacterId::Mita, None).unwrap();
    assert_eq!(snapshot.count(), 142);
    assert_eq!(snapshot.page(3, 0).len(), 3);
    assert_eq!(snapshot.page(3, 0)[0]["timeline_sequence"], 141);
    assert_eq!(snapshot.page(3, 2)[0]["timeline_sequence"], 139);
    assert_eq!(snapshot.page(2, 200).len(), 0);
    assert_eq!(snapshot.page(0, 0).len(), 0);
    let fork = BranchId::new();
    repository.fork(root, checkpoint_id, fork).unwrap();
    assert_eq!(
        TimelineMessageQueries::capture(repository, fork, CharacterId::Mita, None)
            .unwrap()
            .count(),
        0
    );
    assert_eq!(snapshot.count(), 142);
}

#[test]
fn projection_reads_multiple_pages_and_freezes_branch_prefix() {
    let mut repository = TimelineStore::new();
    run_paging_contract(&mut repository);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_projection_reads_multiple_pages_and_freezes_branch_prefix() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path =
        std::env::temp_dir().join(format!("mitaview-paging-{}.sqlite", uuid::Uuid::new_v4()));
    {
        let mut repository = SqliteTimelineRepository::open(&path).unwrap();
        run_paging_contract(&mut repository);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
