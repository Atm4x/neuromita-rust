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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: MemoryId,
    pub priority: MemoryPriority,
    pub content: String,
}

fn normalized_content(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl MemoryRecord {
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
        let mut seen = BTreeSet::new();
        for record in self.records.values() {
            if !seen.insert(normalized_content(&record.content)) {
                return Err("duplicate memory content");
            }
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<MemoryRecord> {
        self.records.values().cloned().collect()
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
                if !self.records.contains_key(&record.id) && self.records.len() >= 256 {
                    return Err("memory capacity exceeded");
                }
                self.records.insert(record.id, record.clone());
            }
            MemoryChange::Delete(id) => {
                if self.records.remove(id).is_none() {
                    return Err("unknown memory id");
                }
            }
        }
        Ok(())
    }

    /// Resolves a proposed batch completely before any Timeline append.
    pub fn resolve(&self, commands: &[MemoryCommand]) -> Result<Vec<MemoryChange>, &'static str> {
        let mut next = self.clone();
        let mut changes = Vec::new();
        for command in commands {
            match command {
                MemoryCommand::Add { priority, content } => {
                    if let Some(previous) = next.records.values().find(|record| {
                        normalized_content(&record.content) == normalized_content(content)
                    }) {
                        if *priority > previous.priority {
                            let mut upgraded = previous.clone();
                            upgraded.priority = *priority;
                            let change = MemoryChange::Upsert(upgraded);
                            next.apply_committed(&change)?;
                            changes.push(change);
                        }
                        continue;
                    }
                    let record = MemoryRecord {
                        id: MemoryId::new(),
                        priority: *priority,
                        content: content.clone(),
                    };
                    let change = MemoryChange::Upsert(record);
                    next.apply_committed(&change)?;
                    changes.push(change);
                }
                MemoryCommand::Update {
                    id,
                    priority,
                    content,
                } => {
                    let mut record = next.get(*id).ok_or("unknown memory id")?.clone();
                    record.content = content.clone();
                    if next.records.values().any(|other| {
                        other.id != *id
                            && normalized_content(&other.content) == normalized_content(content)
                    }) {
                        return Err("duplicate memory content");
                    }
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
                        combined.push(
                            next.get(*source)
                                .ok_or("unknown merge source")?
                                .content
                                .clone(),
                        );
                    }
                    target_record.content = content.clone().unwrap_or_else(|| combined.join(" | "));
                    if next.records.values().any(|other| {
                        !unique.contains(&other.id)
                            && normalized_content(&other.content)
                                == normalized_content(&target_record.content)
                    }) {
                        return Err("duplicate memory content");
                    }
                    for source in sources {
                        let removed = MemoryChange::Delete(*source);
                        next.apply_committed(&removed)?;
                        changes.push(removed);
                    }
                    let updated = MemoryChange::Upsert(target_record);
                    next.apply_committed(&updated)?;
                    changes.push(updated);
                }
            }
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
}
