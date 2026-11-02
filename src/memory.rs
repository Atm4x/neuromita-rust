use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryId(pub Uuid);

impl MemoryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MemoryId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

/// Whether a retained memory participates in ordinary prompt selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryState {
    #[default]
    Active,
    Forgotten,
    Deleted,
}

/// One canonical island per character and category, independently of the fact budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryIsland {
    Relationship,
    Opinion,
    Preferences,
    CommitmentsConflicts,
    Language,
}

impl MemoryIsland {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relationship => "relationship",
            Self::Opinion => "opinion",
            Self::Preferences => "preferences",
            Self::CommitmentsConflicts => "commitments_conflicts",
            Self::Language => "language",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    #[default]
    Fact,
    Island(MemoryIsland),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub priority: MemoryPriority,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_fact")]
    pub kind: MemoryKind,
    #[serde(default, skip_serializing_if = "is_active")]
    pub state: MemoryState,
    /// Explicit expiration cutoff, applied only by a committed maintenance operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_ms: Option<u64>,
}

fn is_fact(kind: &MemoryKind) -> bool {
    *kind == MemoryKind::Fact
}

fn is_active(state: &MemoryState) -> bool {
    *state == MemoryState::Active
}

fn normalized_content(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl MemoryRecord {
    pub fn fact(id: MemoryId, priority: MemoryPriority, content: impl Into<String>) -> Self {
        Self {
            id,
            priority,
            content: content.into(),
            kind: MemoryKind::Fact,
            state: MemoryState::Active,
            expires_at_unix_ms: None,
        }
    }

    pub fn available_at(&self, unix_ms: u64) -> bool {
        self.state == MemoryState::Active
            && self
                .expires_at_unix_ms
                .is_none_or(|expires| expires > unix_ms)
    }

    pub fn is_valid(&self) -> bool {
        let content = self.content.trim();
        !content.is_empty() && content.chars().count() <= 2000
    }
}

/// The model's uncommitted intent, addressed by immutable memory identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryCommand {
    Add {
        priority: MemoryPriority,
        content: String,
    },
    Update {
        id: MemoryId,
        priority: Option<MemoryPriority>,
        content: String,
    },
    Delete {
        id: MemoryId,
    },
    Merge {
        target: MemoryId,
        sources: Vec<MemoryId>,
        content: Option<String>,
    },
    /// Explicitly replace a typed island, preserving its identity on subsequent updates.
    SetIsland {
        island: MemoryIsland,
        content: String,
    },
    Forget {
        id: MemoryId,
    },
    Restore {
        id: MemoryId,
    },
    SoftDelete {
        id: MemoryId,
    },
    SetExpiry {
        id: MemoryId,
        expires_at_unix_ms: Option<u64>,
    },
}

/// Canonical committed operation; upserts include complete values, not partial deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MemoryChange {
    Upsert(MemoryRecord),
    Delete(MemoryId),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharacterMemories {
    records: BTreeMap<MemoryId, MemoryRecord>,
}

impl CharacterMemories {
    pub(crate) fn ensure_unique(&self) -> Result<(), &'static str> {
        let mut facts = BTreeSet::new();
        let mut islands = BTreeSet::new();
        for record in self.records.values() {
            if record.state == MemoryState::Deleted {
                continue;
            }
            match record.kind {
                MemoryKind::Fact => {
                    if !facts.insert(normalized_content(&record.content)) {
                        return Err("duplicate memory content");
                    }
                }
                MemoryKind::Island(island) => {
                    if !islands.insert(island) {
                        return Err("duplicate memory island");
                    }
                }
            }
        }
        Ok(())
    }

    /// Current active, non-expired facts are projected by explicit maintenance;
    /// reading cannot mutate the authoritative Timeline.
    pub fn list(&self) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| record.kind == MemoryKind::Fact && record.state == MemoryState::Active)
            .cloned()
            .collect()
    }

    /// Complete retained history for inspection, restore, and maintenance.
    pub fn all(&self) -> Vec<MemoryRecord> {
        self.records.values().cloned().collect()
    }

    pub fn islands(&self) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| {
                matches!(record.kind, MemoryKind::Island(_)) && record.state == MemoryState::Active
            })
            .cloned()
            .collect()
    }

    pub fn forgotten(&self) -> Vec<MemoryRecord> {
        self.records
            .values()
            .filter(|record| {
                record.kind == MemoryKind::Fact && record.state == MemoryState::Forgotten
            })
            .cloned()
            .collect()
    }

    pub fn get(&self, id: MemoryId) -> Option<&MemoryRecord> {
        self.records.get(&id)
    }

    pub fn apply_committed(&mut self, change: &MemoryChange) -> Result<(), &'static str> {
        match change {
            MemoryChange::Upsert(record) => {
                if !record.is_valid() {
                    return Err("invalid memory record");
                }
                if self
                    .records
                    .get(&record.id)
                    .is_some_and(|existing| existing.kind != record.kind)
                {
                    return Err("memory kind cannot change for an existing id");
                }
                if !self.records.contains_key(&record.id) && self.records.len() >= 1024 {
                    return Err("retained memory capacity exceeded");
                }
                let next_active_facts = self
                    .records
                    .values()
                    .filter(|other| {
                        other.id != record.id
                            && other.kind == MemoryKind::Fact
                            && other.state == MemoryState::Active
                    })
                    .count()
                    + usize::from(
                        record.kind == MemoryKind::Fact && record.state == MemoryState::Active,
                    );
                if next_active_facts > 256 {
                    return Err("active memory capacity exceeded");
                }
                self.records.insert(record.id, record.clone());
            }
            // Legacy physical delete is kept for exact replay compatibility.
            MemoryChange::Delete(id) => {
                if self.records.remove(id).is_none() {
                    return Err("unknown memory id");
                }
            }
        }
        Ok(())
    }

    /// Resolve an explicit TTL maintenance pass without reading the wall clock.
    /// Callers commit the returned changes through the ordinary Timeline boundary.
    pub fn resolve_expirations(&self, as_of_unix_ms: u64) -> Vec<MemoryChange> {
        self.records
            .values()
            .filter(|record| {
                record.state == MemoryState::Active
                    && record
                        .expires_at_unix_ms
                        .is_some_and(|expiry| expiry <= as_of_unix_ms)
            })
            .map(|record| {
                let mut expired = record.clone();
                expired.state = MemoryState::Forgotten;
                MemoryChange::Upsert(expired)
            })
            .collect()
    }

    /// Resolves a proposed batch completely before any Timeline append.
    pub fn resolve(&self, commands: &[MemoryCommand]) -> Result<Vec<MemoryChange>, &'static str> {
        let mut next = self.clone();
        let mut changes = Vec::new();
        for command in commands {
            match command {
                MemoryCommand::Add { priority, content } => {
                    if let Some(previous) = next.records.values().find(|record| {
                        record.kind == MemoryKind::Fact
                            && record.state != MemoryState::Deleted
                            && normalized_content(&record.content) == normalized_content(content)
                    }) {
                        if previous.state != MemoryState::Active {
                            return Err("memory is forgotten: restore explicitly");
                        }
                        if *priority > previous.priority {
                            let mut upgraded = previous.clone();
                            upgraded.priority = *priority;
                            let change = MemoryChange::Upsert(upgraded);
                            next.apply_committed(&change)?;
                            changes.push(change);
                        }
                        continue;
                    }
                    let change = MemoryChange::Upsert(MemoryRecord::fact(
                        MemoryId::new(),
                        *priority,
                        content.clone(),
                    ));
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Update {
                    id,
                    priority,
                    content,
                } => {
                    let mut record = next.get(*id).ok_or("unknown memory id")?.clone();
                    if record.state != MemoryState::Active || record.kind != MemoryKind::Fact {
                        return Err("memory is not an active fact");
                    }
                    record.content = content.clone();
                    if let Some(priority) = priority {
                        record.priority = *priority;
                    }
                    let change = MemoryChange::Upsert(record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Delete { id } => {
                    let change = MemoryChange::Delete(*id);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Merge {
                    target,
                    sources,
                    content,
                } => {
                    let mut target_record =
                        next.get(*target).ok_or("unknown merge target")?.clone();
                    if target_record.kind != MemoryKind::Fact
                        || target_record.state != MemoryState::Active
                    {
                        return Err("merge requires active facts");
                    }
                    if sources.is_empty() {
                        return Err("merge requires sources");
                    }
                    let mut unique = BTreeSet::new();
                    unique.insert(*target);
                    let mut combined = vec![target_record.content.clone()];
                    for source in sources {
                        if !unique.insert(*source) {
                            return Err("duplicate merge memory id");
                        }
                        let source_record = next.get(*source).ok_or("unknown merge source")?;
                        if source_record.kind != MemoryKind::Fact
                            || source_record.state != MemoryState::Active
                        {
                            return Err("merge requires active facts");
                        }
                        combined.push(source_record.content.clone());
                    }
                    target_record.content = content.clone().unwrap_or_else(|| combined.join(" | "));
                    for source in sources {
                        let change = MemoryChange::Delete(*source);
                        next.apply_committed(&change)?;
                        changes.push(change);
                    }
                    let change = MemoryChange::Upsert(target_record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::SetIsland { island, content } => {
                    let record = next
                        .records
                        .values()
                        .find(|record| {
                            record.kind == MemoryKind::Island(*island)
                                && record.state != MemoryState::Deleted
                        })
                        .cloned();
                    if record
                        .as_ref()
                        .is_some_and(|item| item.state != MemoryState::Active)
                    {
                        return Err("forgotten island must be restored explicitly");
                    }
                    let mut record = record.unwrap_or_else(|| MemoryRecord {
                        kind: MemoryKind::Island(*island),
                        ..MemoryRecord::fact(MemoryId::new(), MemoryPriority::Normal, "")
                    });
                    record.content = content.clone();
                    record.state = MemoryState::Active;
                    let change = MemoryChange::Upsert(record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Forget { id }
                | MemoryCommand::Restore { id }
                | MemoryCommand::SoftDelete { id } => {
                    let record = next.get(*id).ok_or("unknown memory id")?;
                    let desired = match command {
                        MemoryCommand::Forget { .. } => MemoryState::Forgotten,
                        MemoryCommand::Restore { .. } => MemoryState::Active,
                        MemoryCommand::SoftDelete { .. } => MemoryState::Deleted,
                        _ => unreachable!(),
                    };
                    if record.state == desired {
                        continue;
                    }
                    if desired == MemoryState::Forgotten && record.state != MemoryState::Active {
                        return Err("only active memory may be forgotten");
                    }
                    let mut changed = record.clone();
                    changed.state = desired;
                    let change = MemoryChange::Upsert(changed);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::SetExpiry {
                    id,
                    expires_at_unix_ms,
                } => {
                    let record = next.get(*id).ok_or("unknown memory id")?;
                    if record.state == MemoryState::Deleted {
                        return Err("deleted memory cannot be rescheduled");
                    }
                    let mut changed = record.clone();
                    changed.expires_at_unix_ms = *expires_at_unix_ms;
                    let change = MemoryChange::Upsert(changed);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
            }
            next.ensure_unique()?;
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_add_coalesces_identity_and_can_upgrade_priority() {
        let id = MemoryId::new();
        let mut book = CharacterMemories::default();
        book.apply_committed(&MemoryChange::Upsert(MemoryRecord {
            id,
            priority: MemoryPriority::Low,
            content: "Player likes TEA".into(),
            kind: crate::memory::MemoryKind::Fact,
            state: crate::memory::MemoryState::Active,
            expires_at_unix_ms: None,
        }))
        .unwrap();
        let changes = book
            .resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: " player   likes tea ".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Low,
                    content: "PLAYER likes tea".into(),
                },
            ])
            .unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0],
            MemoryChange::Upsert(MemoryRecord {
                id,
                priority: MemoryPriority::Normal,
                content: "Player likes TEA".into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                expires_at_unix_ms: None,
            })
        );
        book.apply_committed(&changes[0]).unwrap();
        assert_eq!(book.list().len(), 1);
        assert_eq!(book.get(id).unwrap().priority, MemoryPriority::Normal);
    }

    #[test]
    fn exact_duplicates_inside_batch_do_not_allocate_multiple_ids() {
        let changes = CharacterMemories::default()
            .resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Remember me".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "  remember   ME ".into(),
                },
            ])
            .unwrap();
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn update_to_duplicate_fails_without_mutating_existing_snapshot() {
        let left = MemoryId::new();
        let right = MemoryId::new();
        let mut book = CharacterMemories::default();
        for (id, content) in [(left, "tea"), (right, "coffee")] {
            book.apply_committed(&MemoryChange::Upsert(MemoryRecord {
                id,
                priority: MemoryPriority::Normal,
                content: content.into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                expires_at_unix_ms: None,
            }))
            .unwrap();
        }
        let old = book.clone();
        assert!(
            book.resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Critical,
                    content: "new memory".into(),
                },
                MemoryCommand::Update {
                    id: right,
                    priority: None,
                    content: " TEA ".into(),
                },
            ])
            .is_err()
        );
        assert_eq!(book, old);
    }

    #[test]
    fn merge_is_atomic_and_preserves_target_identity() {
        let a = MemoryId::new();
        let b = MemoryId::new();
        let mut book = CharacterMemories::default();
        for (id, content) in [(a, "first"), (b, "second")] {
            book.apply_committed(&MemoryChange::Upsert(MemoryRecord {
                id,
                priority: MemoryPriority::Normal,
                content: content.into(),
                kind: crate::memory::MemoryKind::Fact,
                state: crate::memory::MemoryState::Active,
                expires_at_unix_ms: None,
            }))
            .unwrap();
        }
        let edits = book
            .resolve(&[MemoryCommand::Merge {
                target: a,
                sources: vec![b],
                content: None,
            }])
            .unwrap();
        for edit in edits {
            book.apply_committed(&edit).unwrap();
        }
        assert_eq!(book.list().len(), 1);
        assert_eq!(book.get(a).unwrap().content, "first | second");
        assert!(book.get(b).is_none());
        assert!(book.resolve(&[MemoryCommand::Delete { id: b }]).is_err());
    }
    fn apply(book: &mut CharacterMemories, commands: &[MemoryCommand]) {
        let changes = book.resolve(commands).expect("valid memory batch");
        for change in changes {
            book.apply_committed(&change).expect("committed change");
        }
    }

    #[test]
    fn legacy_record_json_is_stable_and_extensions_are_strict() {
        let id = MemoryId(Uuid::from_u128(17));
        let legacy = serde_json::json!({"id": id, "priority": "normal", "content": "Tea"});
        let record: MemoryRecord = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(record.kind, MemoryKind::Fact);
        assert_eq!(record.state, MemoryState::Active);
        assert_eq!(record.expires_at_unix_ms, None);
        assert_eq!(serde_json::to_value(&record).unwrap(), legacy);
        assert!(
            serde_json::from_value::<MemoryRecord>(serde_json::json!({
                "id": id, "priority": "normal", "content": "Tea", "unknown": 1
            }))
            .is_err()
        );
    }

    #[test]
    fn forgotten_deleted_and_restored_are_distinct_explicit_states() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Player likes tea".into(),
            }],
        );
        let id = book.list()[0].id;
        apply(&mut book, &[MemoryCommand::Forget { id }]);
        assert!(book.list().is_empty());
        assert_eq!(book.forgotten()[0].id, id);
        assert!(
            book.resolve(&[MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "PLAYER LIKES TEA".into()
            }])
            .is_err()
        );
        apply(&mut book, &[MemoryCommand::Restore { id }]);
        assert_eq!(book.list()[0].id, id);
        apply(&mut book, &[MemoryCommand::SoftDelete { id }]);
        assert!(book.list().is_empty());
        assert!(book.forgotten().is_empty());
        assert_eq!(book.all().len(), 1);
        assert_eq!(book.get(id).unwrap().state, MemoryState::Deleted);
        apply(&mut book, &[MemoryCommand::Restore { id }]);
        assert_eq!(book.list()[0].id, id);
        apply(&mut book, &[MemoryCommand::SoftDelete { id }]);
        apply(&mut book, &[MemoryCommand::Delete { id }]);
        assert!(book.all().is_empty());
    }

    #[test]
    fn islands_are_typed_singletons_separate_from_fact_capacity() {
        let mut book = CharacterMemories::default();
        let islands = [
            MemoryIsland::Relationship,
            MemoryIsland::Opinion,
            MemoryIsland::Preferences,
            MemoryIsland::CommitmentsConflicts,
            MemoryIsland::Language,
        ];
        for island in islands {
            apply(
                &mut book,
                &[MemoryCommand::SetIsland {
                    island,
                    content: format!("first {island:?}"),
                }],
            );
        }
        let original = book
            .islands()
            .iter()
            .map(|item| (item.kind, item.id))
            .collect::<BTreeMap<_, _>>();
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Preferences,
                content: "updated preferences".into(),
            }],
        );
        assert_eq!(book.islands().len(), 5);
        assert_eq!(
            book.islands()
                .iter()
                .map(|item| (item.kind, item.id))
                .collect::<BTreeMap<_, _>>(),
            original
        );
        assert!(book.list().is_empty());
        assert_eq!(
            book.islands()
                .iter()
                .find(|r| r.kind == MemoryKind::Island(MemoryIsland::Preferences))
                .unwrap()
                .content,
            "updated preferences"
        );
        let id = original[&MemoryKind::Island(MemoryIsland::Preferences)];
        assert!(
            book.resolve(&[MemoryCommand::Merge {
                target: id,
                sources: vec![original[&MemoryKind::Island(MemoryIsland::Language)]],
                content: None
            }])
            .is_err()
        );
    }

    #[test]
    fn expiry_is_explicit_idempotent_and_never_mutates_read() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content: "temporary fact".into(),
            }],
        );
        let id = book.list()[0].id;
        apply(
            &mut book,
            &[MemoryCommand::SetExpiry {
                id,
                expires_at_unix_ms: Some(1000),
            }],
        );
        assert!(book.get(id).unwrap().available_at(999));
        assert!(!book.get(id).unwrap().available_at(1000));
        assert!(book.resolve_expirations(999).is_empty());
        assert_eq!(book.list().len(), 1);
        let changes = book.resolve_expirations(1000);
        assert_eq!(changes.len(), 1);
        assert_eq!(book.list().len(), 1);
        for change in changes {
            book.apply_committed(&change).unwrap();
        }
        assert!(book.resolve_expirations(2000).is_empty());
        assert!(book.list().is_empty());
        assert_eq!(book.forgotten()[0].id, id);
    }

    #[test]
    fn invalid_batch_has_no_partial_effects_and_unique_island_validation() {
        let mut book = CharacterMemories::default();
        apply(
            &mut book,
            &[MemoryCommand::SetIsland {
                island: MemoryIsland::Language,
                content: "Russian".into(),
            }],
        );
        let initial = book.clone();
        assert!(
            book.resolve(&[
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "tea".into()
                },
                MemoryCommand::SetIsland {
                    island: MemoryIsland::Opinion,
                    content: "".into()
                },
            ])
            .is_err()
        );
        assert_eq!(book, initial);
        let mut duplicate =
            MemoryRecord::fact(MemoryId::new(), MemoryPriority::Normal, "duplicate");
        duplicate.kind = MemoryKind::Island(MemoryIsland::Language);
        book.apply_committed(&MemoryChange::Upsert(duplicate))
            .unwrap();
        assert_eq!(book.ensure_unique(), Err("duplicate memory island"));
    }
}
