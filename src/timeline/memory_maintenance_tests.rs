use serde_json::json;

use super::{
    SessionTimeline,
    domain::{EventDraft, EventKind, TimelineError},
};
use crate::{
    identity::CharacterId,
    memory::{MemoryChange, MemoryId, MemoryPriority, MemoryRecord, MemoryState},
};

fn append_memory(timeline: &mut SessionTimeline, record: MemoryRecord) {
    let mut draft = EventDraft::new(EventKind::CharacterMemoryChanged);
    draft.actor = Some(CharacterId::Mita.to_string());
    draft.observers.insert(CharacterId::Mita);
    draft
        .payload
        .insert("change".into(), json!(MemoryChange::Upsert(record)));
    timeline
        .append_test_fact(&format!("test-memory:{}", draft.id.0), draft)
        .unwrap();
}

#[test]
fn expiry_commits_only_due_unprotected_memories_once_and_is_character_private() {
    let mut timeline = SessionTimeline::new();
    let first = MemoryId::new();
    let protected_id = MemoryId::new();
    let late = MemoryId::new();
    for (id, protected, expiry) in [
        (first, false, 100),
        (protected_id, true, 100),
        (late, false, 200),
    ] {
        let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, id.0.to_string());
        record.protected = protected;
        record.expires_at_unix_ms = Some(expiry);
        append_memory(&mut timeline, record);
    }
    let selected = timeline.selection();
    let initial = timeline.anchor();
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Mita, 99),
        Ok(0)
    );
    assert_eq!(timeline.anchor(), initial);
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Mita, 100),
        Ok(1)
    );
    assert_eq!(timeline.anchor().through, initial.through + 1);
    let state = timeline
        .inspect_context_at(CharacterId::Mita, timeline.anchor())
        .unwrap()
        .memories;
    assert_eq!(state.get(first).unwrap().state, MemoryState::Forgotten);
    assert_eq!(state.get(protected_id).unwrap().state, MemoryState::Active);
    assert_eq!(state.get(late).unwrap().state, MemoryState::Active);
    assert!(
        timeline
            .inspect_context_at(CharacterId::KindMita, timeline.anchor())
            .unwrap()
            .memories
            .all()
            .is_empty()
    );
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Mita, 100),
        Ok(0)
    );
    assert_eq!(
        timeline.expire_character_memories(selected, CharacterId::Mita, 200),
        Ok(1)
    );
}

#[test]
fn wrong_selection_cannot_start_expiration_maintenance() {
    let mut timeline = SessionTimeline::new();
    let old = timeline.selection();
    timeline
        .select_branch(old.branch_id, old)
        .expect("same branch can be reselected");
    assert_eq!(
        timeline.expire_character_memories(old, CharacterId::Mita, 100),
        Err(TimelineError::ConcurrentWrite)
    );
}

#[tokio::test]
async fn startup_maintenance_replays_expiration_without_running_a_scheduler() {
    use std::sync::{Arc, Mutex};

    let mut timeline = SessionTimeline::new();
    let expired = MemoryId::new();
    let protected = MemoryId::new();
    for (id, protected_flag) in [(expired, false), (protected, true)] {
        let mut record = MemoryRecord::fact(id, MemoryPriority::Normal, id.0.to_string());
        record.protected = protected_flag;
        record.expires_at_unix_ms = Some(1);
        append_memory(&mut timeline, record);
    }
    let before = timeline.anchor().through;
    let timeline = Arc::new(Mutex::new(timeline));
    let stop = tokio_util::sync::CancellationToken::new();
    let (_, tasks) =
        crate::character::CharacterRegistry::spawn(Arc::clone(&timeline), stop.clone(), 4);
    {
        let timeline = timeline.lock().unwrap();
        assert_eq!(timeline.anchor().through, before + 1);
        let memories = timeline
            .inspect_context_at(CharacterId::Mita, timeline.anchor())
            .unwrap()
            .memories;
        assert_eq!(memories.get(expired).unwrap().state, MemoryState::Forgotten);
        assert_eq!(memories.get(protected).unwrap().state, MemoryState::Active);
    }
    stop.cancel();
    for task in tasks {
        task.await.unwrap();
    }
}
