use neuromita::{
    character::CharacterId,
    request::RequestId,
    timeline::domain::{
        BranchId, EventDraft, EventKind, SessionId, TimelineError, TimelineFact,
        TimelineRepository, TimelineStore,
    },
};
use serde_json::json;
use uuid::Uuid;

fn input(request: RequestId, character: CharacterId) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft.observers.insert(character);
    draft.payload.insert("content".into(), json!("input"));
    draft
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    draft
}

fn reply(request: RequestId, character: CharacterId) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::CharacterMessage);
    draft.actor = Some(character.to_string());
    draft.observers.insert(character);
    draft
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    draft.payload.insert("content".into(), json!("accepted"));
    draft
}

fn append(
    repo: &mut dyn TimelineRepository,
    branch: BranchId,
    operation: &str,
    draft: EventDraft,
) -> TimelineFact {
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(branch, head, operation, json!({}), vec![draft])
        .unwrap()
        .pop()
        .unwrap()
}

#[test]
fn recovery_uses_only_accepted_replies_not_admission_or_reset() {
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let request = RequestId(Uuid::new_v4());
    let incoming = append(&mut repo, branch, "in", input(request, CharacterId::Mita));
    let accepted = |repo: &dyn TimelineRepository, at| {
        repo.accepted_reply_at(branch, CharacterId::Mita, request.0, at)
            .unwrap()
    };
    assert!(accepted(&repo, None).is_none());
    assert!(accepted(&repo, Some(incoming.sequence)).is_none());

    let mut proactive = EventDraft::new(EventKind::ProactiveOpportunity);
    proactive.actor = Some(CharacterId::Mita.to_string());
    proactive.observers.insert(CharacterId::Mita);
    proactive
        .payload
        .insert("request_id".into(), json!(request.to_string()));
    append(&mut repo, branch, "proactive", proactive);
    assert!(accepted(&repo, None).is_none());

    let committed = append(
        &mut repo,
        branch,
        "accepted",
        reply(request, CharacterId::Mita),
    );
    assert_eq!(accepted(&repo, None), Some(committed.clone()));
    assert!(accepted(&repo, Some(committed.sequence - 1)).is_none());
    assert_eq!(
        repo.accepted_reply_at(branch, CharacterId::KindMita, request.0, None)
            .unwrap(),
        None
    );
    assert_eq!(
        repo.accepted_reply_at(branch, CharacterId::Mita, Uuid::new_v4(), None)
            .unwrap(),
        None
    );

    let mut reset = EventDraft::new(EventKind::CharacterContextReset);
    reset.observers.insert(CharacterId::Mita);
    append(&mut repo, branch, "reset", reset);
    assert_eq!(accepted(&repo, None), Some(committed));
    assert!(matches!(
        repo.accepted_reply_at(branch, CharacterId::Mita, request.0, Some(1_000)),
        Err(TimelineError::Invalid(_))
    ));
}

#[test]
fn recovery_follows_fork_cutoff_and_detects_duplicate_commit() {
    let mut repo = TimelineStore::new();
    let root = BranchId::new();
    let child = BranchId::new();
    repo.create_session(SessionId::new(), root).unwrap();
    let prior = RequestId(Uuid::new_v4());
    let future = RequestId(Uuid::new_v4());
    let committed = append(&mut repo, root, "prior", reply(prior, CharacterId::Mita));
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(&mut repo, root, "checkpoint", checkpoint);
    repo.fork(root, checkpoint_id, child).unwrap();
    append(&mut repo, root, "future", reply(future, CharacterId::Mita));
    assert_eq!(
        repo.accepted_reply_at(child, CharacterId::Mita, prior.0, None)
            .unwrap()
            .unwrap()
            .draft
            .id,
        committed.draft.id
    );
    assert!(
        repo.accepted_reply_at(child, CharacterId::Mita, future.0, None)
            .unwrap()
            .is_none()
    );
    append(
        &mut repo,
        child,
        "branch-reply",
        reply(future, CharacterId::Mita),
    );
    assert!(
        repo.accepted_reply_at(child, CharacterId::Mita, future.0, None)
            .unwrap()
            .is_some()
    );
    append(
        &mut repo,
        child,
        "duplicate",
        reply(future, CharacterId::Mita),
    );
    assert_eq!(
        repo.accepted_reply_at(child, CharacterId::Mita, future.0, None),
        Err(TimelineError::Conflict)
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_reopen_reconciles_uncertain_commits_without_replaying_them() {
    use neuromita::timeline::storage::SqliteTimelineRepository;

    let path = std::env::temp_dir().join(format!("neuromita-recovery-{}.db", Uuid::new_v4()));
    let branch = BranchId::new();
    let session = SessionId::new();
    let accepted = RequestId(Uuid::new_v4());
    let unaccepted = RequestId(Uuid::new_v4());
    let committed = {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(session, branch).unwrap();
        append(
            &mut repo,
            branch,
            "admitted",
            input(unaccepted, CharacterId::Mita),
        );
        append(
            &mut repo,
            branch,
            "input",
            input(accepted, CharacterId::Mita),
        );
        append(
            &mut repo,
            branch,
            "accepted",
            reply(accepted, CharacterId::Mita),
        )
    };
    for _ in 0..2 {
        let repo = SqliteTimelineRepository::open(&path).unwrap();
        let before = repo.branch(branch).unwrap().head_sequence;
        assert_eq!(
            repo.accepted_reply_at(branch, CharacterId::Mita, accepted.0, None)
                .unwrap(),
            Some(committed.clone())
        );
        assert!(
            repo.accepted_reply_at(branch, CharacterId::Mita, unaccepted.0, None)
                .unwrap()
                .is_none()
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, before);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
