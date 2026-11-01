#![cfg(feature = "sqlite-system")]

use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{
            BranchId, CausalLink, CausalRelation, EventDraft, EventKind, SessionId, TimelineError,
            TimelineRepository, TimelineStore,
        },
        storage::SqliteTimelineRepository,
    },
};
use serde_json::json;
use uuid::Uuid;

struct DatabaseFile(PathBuf);

impl DatabaseFile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("neuromita-timeline-{}.db", Uuid::new_v4()));
        Self(path)
    }
    fn open(&self) -> SqliteTimelineRepository {
        SqliteTimelineRepository::open(&self.0).unwrap()
    }
}
impl Drop for DatabaseFile {
    fn drop(&mut self) {
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
    }
}

fn mk(kind: EventKind, observers: &[CharacterId]) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.observers.extend(observers.iter().copied());
    draft
}
fn append<T: TimelineRepository + ?Sized>(
    repo: &mut T,
    id: BranchId,
    key: &str,
    draft: EventDraft,
) {
    let head = repo.branch(id).unwrap().head_sequence;
    repo.append(id, head, key, json!({"operation": key}), vec![draft])
        .unwrap();
}

fn contract(repo: &mut dyn TimelineRepository) {
    let session = SessionId::new();
    let root = BranchId::new();
    let info = repo.create_session(session, root).unwrap();
    assert_eq!(repo.create_session(session, root), Ok(info));
    let mut first = mk(EventKind::UnityEvent, &[CharacterId::Mita]);
    first
        .payload
        .insert("value".into(), json!({"nested": [2, 4]}));
    let first_id = first.id;
    let original = repo
        .append(root, 0, "first", json!({"a": 1}), vec![first.clone()])
        .unwrap();
    let mut replay = first;
    replay.occurred_at_unix_ms += 1000;
    append(
        repo,
        root,
        "advance",
        mk(EventKind::UnityEvent, &[CharacterId::Mita]),
    );
    assert_eq!(
        repo.append(root, 0, "first", json!({"a": 1}), vec![replay.clone()]),
        Ok(original)
    );
    assert_eq!(
        repo.append(root, 0, "first", json!({"a": 2}), vec![replay]),
        Err(TimelineError::Conflict)
    );
    assert!(repo.knows(root, CharacterId::Mita, first_id, None).unwrap());
    assert!(
        !repo
            .knows(root, CharacterId::KindMita, first_id, None)
            .unwrap()
    );
    let mut disclosure = mk(EventKind::EventObserved, &[CharacterId::KindMita]);
    disclosure.links.push(CausalLink {
        relation: CausalRelation::Observes,
        event_id: first_id,
    });
    append(repo, root, "disclosure", disclosure);
    assert_eq!(
        repo.view(root, CharacterId::KindMita, None).unwrap()[0].learned_at,
        3
    );
    let mut checkpoint = mk(EventKind::Checkpoint, &[]);
    checkpoint
        .payload
        .insert("snapshot".into(), json!({"player": {"level": 7}}));
    let checkpoint_id = checkpoint.id;
    append(repo, root, "checkpoint", checkpoint);
    let child = BranchId::new();
    let info = repo.fork(root, checkpoint_id, child).unwrap();
    assert_eq!(repo.fork(root, checkpoint_id, child), Ok(info));
    let old_future = mk(EventKind::UnityEvent, &[CharacterId::Mita]);
    let old_future_id = old_future.id;
    append(repo, root, "future", old_future);
    assert!(
        !repo
            .world(child)
            .unwrap()
            .iter()
            .any(|fact| fact.draft.id == old_future_id)
    );
    assert!(
        repo.world(root)
            .unwrap()
            .iter()
            .any(|fact| fact.draft.id == old_future_id)
    );
    assert_eq!(repo.world(child).unwrap().len(), 5);
}

#[test]
fn in_memory_and_sqlite_obey_the_same_domain_contract() {
    contract(&mut TimelineStore::new());
    let db = DatabaseFile::new();
    contract(&mut db.open());
}

#[test]
fn committed_facts_and_receipts_survive_reopen() {
    let db = DatabaseFile::new();
    let session = SessionId::new();
    let branch = BranchId::new();
    let mut original = db.open();
    original.create_session(session, branch).unwrap();
    let draft = mk(EventKind::UnityEvent, &[CharacterId::Mita]);
    let committed = original
        .append(branch, 0, "once", json!({"id": 1}), vec![draft.clone()])
        .unwrap();
    drop(original);
    let mut reopened = db.open();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 1);
    assert_eq!(reopened.world(branch).unwrap(), committed);
    assert_eq!(
        reopened.append(branch, 0, "once", json!({"id": 1}), vec![draft]),
        Ok(committed)
    );
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 1);
    assert_eq!(
        reopened.create_session(session, BranchId::new()),
        Err(TimelineError::Conflict)
    );
}

#[test]
fn invalid_batch_cannot_leave_partial_events_or_receipts() {
    let db = DatabaseFile::new();
    let branch = BranchId::new();
    let mut repo = db.open();
    repo.create_session(SessionId::new(), branch).unwrap();
    let invalid = EventDraft::new(EventKind::CharacterMessage);
    assert!(
        repo.append(
            branch,
            0,
            "failed",
            json!({}),
            vec![mk(EventKind::UnityEvent, &[]), invalid]
        )
        .is_err()
    );
    drop(repo);
    let mut reopened = db.open();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 0);
    assert!(reopened.world(branch).unwrap().is_empty());
    assert_eq!(
        reopened.append(branch, 0, "failed", json!({}), vec![]),
        Ok(Vec::new())
    );
}

#[test]
fn two_independent_connections_cannot_commit_the_same_expected_head() {
    let db = DatabaseFile::new();
    let branch = BranchId::new();
    db.open().create_session(SessionId::new(), branch).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for name in ["a", "b"] {
        let barrier = Arc::clone(&barrier);
        let path = db.0.clone();
        threads.push(thread::spawn(move || {
            let mut repo = SqliteTimelineRepository::open(path).unwrap();
            barrier.wait();
            repo.append(
                branch,
                0,
                name,
                json!({}),
                vec![mk(EventKind::UnityEvent, &[])],
            )
        }));
    }
    let results = threads
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(TimelineError::ConcurrentWrite))
            .count(),
        1
    );
    assert_eq!(db.open().branch(branch).unwrap().head_sequence, 1);
}

#[test]
fn timeline_tables_do_not_conflict_with_other_application_tables() {
    let db = DatabaseFile::new();
    let mut repo = db.open();
    let id = BranchId::new();
    repo.create_session(SessionId::new(), id).unwrap();
    assert_eq!(db.open().branch(id).unwrap().head_sequence, 0);
}

#[test]
fn selection_is_durable_cas_and_reselection_invalidates_previous_pins() {
    use neuromita::timeline::selection::TimelineSelectionRepository;
    let db = DatabaseFile::new();
    let root = BranchId::new();
    let mut first = db.open();
    let mut second = db.open();
    first.create_session(SessionId::new(), root).unwrap();
    assert_eq!(first.selection().unwrap(), None);
    let initial = first.select_branch(root, None).unwrap();
    assert_eq!(second.selection().unwrap(), Some(initial));
    assert_eq!(
        second.select_branch(root, None),
        Err(TimelineError::ConcurrentWrite)
    );
    let next = second.select_branch(root, Some(initial)).unwrap();
    assert_eq!(next.generation, initial.generation + 1);
    assert_eq!(first.selection().unwrap(), Some(next));
    drop(first);
    drop(second);
    assert_eq!(db.open().selection().unwrap(), Some(next));
}

fn page_contract(
    repo: &mut dyn TimelineRepository,
    root: BranchId,
    session: SessionId,
) -> (
    Vec<Vec<neuromita::timeline::domain::TimelineFact>>,
    Vec<Vec<neuromita::timeline::domain::TimelineFact>>,
) {
    repo.create_session(session, root).unwrap();
    for i in 0..9 {
        let observers = if i % 2 == 0 {
            &[CharacterId::Mita][..]
        } else {
            &[CharacterId::KindMita][..]
        };
        append(
            repo,
            root,
            &format!("event:{i}"),
            mk(EventKind::UnityEvent, observers),
        );
    }
    let mut checkpoint = mk(EventKind::Checkpoint, &[]);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(repo, root, "checkpoint", checkpoint);
    let child = BranchId(Uuid::from_u128(0x11111111111111111111111111111111));
    repo.fork(root, checkpoint_id, child).unwrap();
    for i in 0..4 {
        append(
            repo,
            child,
            &format!("child:{i}"),
            mk(EventKind::UnityEvent, &[CharacterId::Mita]),
        );
    }
    append(
        repo,
        root,
        "unrelated parent future",
        mk(EventKind::UnityEvent, &[CharacterId::KindMita]),
    );
    let mut all = Vec::new();
    let mut observed = Vec::new();
    for after in [0, 1, 4, 9, 10, 11, 12, 13] {
        for through in [None, Some(2), Some(9), Some(10), Some(13), Some(15)] {
            for limit in [1, 2, 4, 100] {
                let result = repo.read(child, after, through, limit, None);
                let witness = repo.read(child, after, through, limit, Some(CharacterId::Mita));
                if through.is_some_and(|upper| upper > 15) {
                    assert!(result.is_err());
                    assert!(witness.is_err());
                    continue;
                }
                all.push(result.unwrap());
                observed.push(witness.unwrap());
            }
        }
    }
    (all, observed)
}

#[test]
fn indexed_sqlite_read_matches_in_memory_across_nested_cutoffs_and_direct_witnesses() {
    let root = BranchId::new();
    let session = SessionId::new();
    let (expected, observed) = page_contract(&mut TimelineStore::new(), root, session);
    let db = DatabaseFile::new();
    let (actual, sqlite_observed) = page_contract(&mut db.open(), root, session);
    let signature = |pages: Vec<Vec<neuromita::timeline::domain::TimelineFact>>| {
        pages
            .into_iter()
            .map(|page| {
                page.into_iter()
                    .map(|fact| (fact.sequence, fact.draft.kind, fact.draft.observers))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(signature(actual), signature(expected));
    assert_eq!(signature(sqlite_observed), signature(observed));
}

#[test]
fn workspace_pins_validate_against_sqlite_without_a_timeline_store_dependency() {
    use neuromita::timeline::selection::SessionWorkspace;
    let db = DatabaseFile::new();
    let root = BranchId::new();
    let mut repo = db.open();
    repo.create_session(SessionId::new(), root).unwrap();
    let mut workspace = SessionWorkspace::default();
    workspace.select(&repo, root, None).unwrap();
    let pin = workspace.capture(&repo, CharacterId::Mita, None).unwrap();
    append(&mut repo, root, "unrelated", mk(EventKind::UnityEvent, &[]));
    assert_eq!(workspace.assert_current(&repo, pin), Ok(()));
    append(
        &mut repo,
        root,
        "reset",
        mk(EventKind::CharacterContextReset, &[CharacterId::Mita]),
    );
    assert_eq!(
        workspace.assert_current(&repo, pin),
        Err(TimelineError::ConcurrentWrite)
    );
}
