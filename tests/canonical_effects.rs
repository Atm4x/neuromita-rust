use neuromita::{
    character::CharacterId,
    memory::{MemoryChange, MemoryId, MemoryPriority, MemoryRecord},
    timeline::domain::{
        BranchId, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore,
    },
};
use serde_json::{Value, json};

fn effect(kind: EventKind, payload: Value) -> EventDraft {
    let mut draft = EventDraft::new(kind);
    draft.actor = Some(CharacterId::Mita.to_string());
    draft.observers.insert(CharacterId::Mita);
    draft.payload = payload.as_object().unwrap().clone();
    draft
}

fn push(store: &mut dyn TimelineRepository, branch: BranchId, drafts: Vec<EventDraft>) -> bool {
    let head = store.branch(branch).unwrap().head_sequence;
    let key = uuid::Uuid::new_v4().to_string();
    store.append(branch, head, &key, json!({}), drafts).is_ok()
}

fn check(store: &mut dyn TimelineRepository) {
    let branch = BranchId::new();
    store.create_session(SessionId::new(), branch).unwrap();
    for kind in [
        EventKind::CharacterStateChanged,
        EventKind::CharacterWorkingStateReplaced,
        EventKind::CharacterMemoryChanged,
    ] {
        assert!(!push(store, branch, vec![effect(kind, json!({}))]));
        assert!(!push(
            store,
            branch,
            vec![effect(kind, json!({"wrong":{}}))]
        ));
    }
    assert!(!push(
        store,
        branch,
        vec![effect(
            EventKind::CharacterStateChanged,
            json!({"state":{"attitude":101,"boredom":10,"stress":5}})
        )]
    ));
    assert!(!push(
        store,
        branch,
        vec![effect(
            EventKind::CharacterStateChanged,
            json!({"state":{"attitude":50,"boredom":10,"stress":5,"extra":true}})
        )]
    ));
    assert!(!push(
        store,
        branch,
        vec![effect(
            EventKind::CharacterWorkingStateReplaced,
            json!({"working_state":{"focus":"a".repeat(601)}})
        )]
    ));
    let mut public = effect(
        EventKind::CharacterWorkingStateReplaced,
        json!({"working_state":{}}),
    );
    public.observers.insert(CharacterId::KindMita);
    assert!(!push(store, branch, vec![public]));
    assert_eq!(store.branch(branch).unwrap().head_sequence, 0);

    let record = MemoryRecord {
        id: MemoryId::new(),
        priority: MemoryPriority::High,
        content: "Player likes tea".into(),
    };
    let memory =
        |change: MemoryChange| effect(EventKind::CharacterMemoryChanged, json!({"change":change}));
    assert!(!push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Upsert(record.clone()))]
    ));
    let before = store.branch(branch).unwrap().head_sequence;
    let mut duplicate = record.clone();
    duplicate.id = MemoryId::new();
    duplicate.content = " PLAYER  likes TEA ".into();
    assert!(!push(
        store,
        branch,
        vec![memory(MemoryChange::Upsert(duplicate))]
    ));
    assert!(!push(
        store,
        branch,
        vec![
            memory(MemoryChange::Delete(record.id)),
            memory(MemoryChange::Delete(record.id))
        ]
    ));
    assert_eq!(store.branch(branch).unwrap().head_sequence, before);
    assert_eq!(store.world(branch).unwrap().len(), before as usize);
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(!push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));

    let mut batch = Vec::new();
    for index in 0..257 {
        batch.push(memory(MemoryChange::Upsert(MemoryRecord {
            id: MemoryId::new(),
            priority: MemoryPriority::Normal,
            content: format!("memory {index}"),
        })));
    }
    let before = store.branch(branch).unwrap().head_sequence;
    assert!(!push(store, branch, batch));
    assert_eq!(store.branch(branch).unwrap().head_sequence, before);

    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let id = checkpoint.id;
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Upsert(record.clone())), checkpoint]
    ));
    let child = BranchId::new();
    store.fork(branch, id, child).unwrap();
    assert!(push(
        store,
        branch,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(push(
        store,
        child,
        vec![memory(MemoryChange::Delete(record.id))]
    ));
    assert!(!push(
        store,
        child,
        vec![memory(MemoryChange::Delete(record.id))]
    ));

    let target = MemoryRecord {
        id: MemoryId::new(),
        priority: MemoryPriority::Normal,
        content: "merge target".into(),
    };
    let source = MemoryRecord {
        id: MemoryId::new(),
        priority: MemoryPriority::High,
        content: "merge source".into(),
    };
    assert!(push(
        store,
        branch,
        vec![
            memory(MemoryChange::Upsert(target.clone())),
            memory(MemoryChange::Upsert(source.clone()))
        ]
    ));
    let mut merged = target;
    merged.content = source.content;
    assert!(push(
        store,
        branch,
        vec![
            memory(MemoryChange::Upsert(merged)),
            memory(MemoryChange::Delete(source.id))
        ]
    ));
}

#[test]
fn all_writers_validate_canonical_effects_and_atomic_memory_batches() {
    check(&mut TimelineStore::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_uses_the_same_canonical_validation_inside_the_transaction() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let path = std::env::temp_dir().join(format!(
        "neuromita-effects-{}.sqlite3",
        uuid::Uuid::new_v4()
    ));
    let mut repository = SqliteTimelineRepository::open(&path).unwrap();
    check(&mut repository);
    drop(repository);
    std::fs::remove_file(path).unwrap();
}
