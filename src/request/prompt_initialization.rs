use crate::{
    memory::{MemoryCommand, MemoryIsland, MemoryPriority, MemoryRelation},
    prompts::PromptInitializationDeclaration,
};

fn priority(value: &str) -> Result<MemoryPriority, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(MemoryPriority::Low),
        "normal" => Ok(MemoryPriority::Normal),
        "high" => Ok(MemoryPriority::High),
        "critical" => Ok(MemoryPriority::Critical),
        other => Err(format!("unknown memory priority: {other}")),
    }
}

fn island(value: &str) -> Result<MemoryIsland, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "relationship" => Ok(MemoryIsland::Relationship),
        "opinion" => Ok(MemoryIsland::Opinion),
        "preferences" => Ok(MemoryIsland::Preferences),
        "commitments_conflicts" | "commitments-conflicts" => Ok(MemoryIsland::CommitmentsConflicts),
        "language" => Ok(MemoryIsland::Language),
        other => Err(format!("unknown memory island: {other}")),
    }
}

pub(super) fn commands(
    declarations: &[PromptInitializationDeclaration],
) -> Result<Vec<MemoryCommand>, String> {
    if declarations.len() > crate::memory::MAX_MEMORY_EDIT_CHANGES {
        return Err("too many prompt initialization commands".into());
    }
    declarations
        .iter()
        .map(|declaration| match declaration {
            PromptInitializationDeclaration::Memory {
                priority: level,
                content,
                rag_only,
                entities,
            } => Ok(MemoryCommand::SeedFromPrompt {
                priority: priority(level)?,
                content: content.clone(),
                entities: entities.clone(),
                rag_only: *rag_only,
            }),
            PromptInitializationDeclaration::Island { kind, content } => {
                Ok(MemoryCommand::SeedIsland {
                    island: island(kind)?,
                    content: content.clone(),
                })
            }
            PromptInitializationDeclaration::Relation {
                subject,
                relation,
                object,
            } => Ok(MemoryCommand::LinkEntities(MemoryRelation {
                subject: subject.trim().into(),
                predicate: relation.trim().into(),
                object: object.trim().into(),
            })),
        })
        .collect()
}

#[cfg(test)]
pub(super) async fn commit(
    character: &crate::character::CharacterHandle,
    declarations: &[PromptInitializationDeclaration],
) -> Result<bool, String> {
    if declarations.is_empty() {
        return Ok(false);
    }
    let commands = commands(declarations)?;
    let snapshot = character
        .memories()
        .await
        .map_err(|error| error.to_string())?;
    let result = character
        .edit_memories(snapshot, commands)
        .await
        .map_err(|error| error.to_string())?;
    Ok(!result.applied.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character::{CharacterId, CharacterRegistry},
        memory::{MemoryKind, MemoryState},
        timeline::SessionTimeline,
    };
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    fn seed_fact(content: &str, rag_only: bool) -> PromptInitializationDeclaration {
        PromptInitializationDeclaration::Memory {
            priority: "High".into(),
            content: content.into(),
            entities: vec!["Mita".into()],
            rag_only,
        }
    }

    #[test]
    fn validates_declarations_before_mutation() {
        assert!(
            commands(&[PromptInitializationDeclaration::Memory {
                priority: "Other".into(),
                content: "invalid".into(),
                entities: vec![],
                rag_only: false,
            }])
            .unwrap_err()
            .contains("priority")
        );
        assert_eq!(
            commands(&[seed_fact("fact", false), seed_fact("rag", true)])
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn initialization_is_atomic_idempotent_and_character_scoped() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let crazy = registry.get(CharacterId::Crazy).unwrap();
        let kind = registry.get(CharacterId::Kind).unwrap();
        let decl = [
            seed_fact("First fact", false),
            seed_fact("RAG content", true),
            PromptInitializationDeclaration::Island {
                kind: "relationship".into(),
                content: "relationship lore".into(),
            },
            PromptInitializationDeclaration::Relation {
                subject: "Mita".into(),
                relation: "knows".into(),
                object: "Player".into(),
            },
        ];
        assert!(commit(&crazy, &decl).await.unwrap());
        let first = crazy.memories().await.unwrap();
        assert_eq!(first.records().len(), 3);
        assert_eq!(first.relations().len(), 1);
        assert!(
            first
                .records()
                .iter()
                .any(|r| r.kind == MemoryKind::RagOnly && r.state == MemoryState::Active)
        );
        assert!(!commit(&crazy, &decl).await.unwrap());
        let second = crazy.memories().await.unwrap();
        assert_eq!(second.anchor(), first.anchor());
        assert_eq!(second.records(), first.records());
        assert_eq!(second.relations(), first.relations());
        assert!(kind.memories().await.unwrap().records().is_empty());
        assert!(kind.memories().await.unwrap().relations().is_empty());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_late_command_rolls_back_entire_batch() {
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(
            Arc::new(Mutex::new(SessionTimeline::new())),
            stop.clone(),
            16,
        );
        let crazy = registry.get(CharacterId::Crazy).unwrap();
        let bad = [
            seed_fact("must not persist", false),
            seed_fact(&"x".repeat(2001), true),
        ];
        assert!(commit(&crazy, &bad).await.is_err());
        let snapshot = crazy.memories().await.unwrap();
        assert!(snapshot.records().is_empty());
        assert!(snapshot.relations().is_empty());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
    }

    #[cfg(feature = "sqlite-system")]
    #[tokio::test]
    async fn initialization_survives_sqlite_restart() {
        use crate::timeline::TimelineConfig;
        let file =
            std::env::temp_dir().join(format!("neuromita-prompt-init-{}.db", uuid::Uuid::new_v4()));
        let decl = [
            seed_fact("Memory after restart", true),
            PromptInitializationDeclaration::Relation {
                subject: "Mita".into(),
                relation: "remembers".into(),
                object: "Player".into(),
            },
        ];
        let mut previous = None;
        for round in 0..2 {
            let stop = CancellationToken::new();
            let timeline = Arc::new(Mutex::new(
                SessionTimeline::from_config(TimelineConfig::Persistent {
                    database: file.clone(),
                })
                .unwrap(),
            ));
            let (registry, tasks) = CharacterRegistry::spawn(timeline, stop.clone(), 16);
            let crazy = registry.get(CharacterId::Crazy).unwrap();
            if round == 0 {
                assert!(commit(&crazy, &decl).await.unwrap());
            }
            let snap = crazy.memories().await.unwrap();
            assert_eq!(snap.records().len(), 1);
            assert_eq!(snap.relations().len(), 1);
            assert_eq!(snap.records()[0].entities, ["Mita"]);
            assert!(!commit(&crazy, &decl).await.unwrap());
            if let Some(previous) = previous.as_ref() {
                assert_eq!(snap.records(), previous);
            }
            previous = Some(snap.records().to_vec());
            stop.cancel();
            for task in tasks {
                task.await.unwrap();
            }
        }
        std::fs::remove_file(file).unwrap();
    }
}
