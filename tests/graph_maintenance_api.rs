use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, GraphMaintenanceApi, GraphMaintenanceApiError},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::{
        KnowledgeRetrieval,
        maintenance::{GraphMaintenanceError, GraphMaintenanceMode},
    },
    memory::{MemoryCommand, MemoryPriority, MemoryRelation},
};
use tokio_util::sync::CancellationToken;

static APPLICATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct UnusedProvider;
impl GenerationProvider for UnusedProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, _: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async { Err(GenerationError::Transport("not requested".into())) })
    }
}

fn config() -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture",
            "unused-key",
            GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let cap = |n| NonZeroUsize::new(n).unwrap();
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([Arc::new(UnusedProvider) as Arc<dyn GenerationProvider>]).unwrap(),
        RuntimeLimits {
            request_queue: cap(4),
            discovery_buffer: cap(8),
            character_commands: cap(16),
            presenter_updates: cap(8),
            retained_finished: cap(8),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.knowledge = KnowledgeRetrieval::lexical();
    config
}

#[tokio::test]
async fn live_graph_clear_is_scoped_confirmed_versioned_and_preserves_canonical_fact() {
    let _single_app = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let kind = CharacterApi::get(CharacterId::Kind).unwrap();
    let initial = crazy.memories().await.unwrap();
    let added = crazy
        .edit_memories(
            initial,
            vec![MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Mita likes green tea".into(),
            }],
        )
        .await
        .unwrap();
    let id = added.snapshot.records()[0].id;
    let relation = MemoryRelation {
        subject: "Mita".into(),
        predicate: "likes".into(),
        object: "green tea".into(),
    };
    let indexed = crazy
        .edit_memories(
            added.snapshot.clone(),
            vec![MemoryCommand::ReplaceExtractedRelations {
                source: id,
                expected_revision: added.snapshot.source_revision(id).unwrap(),
                relations: vec![relation.clone()],
            }],
        )
        .await
        .unwrap();
    assert_eq!(indexed.snapshot.extracted_relations().len(), 1);

    let preview =
        GraphMaintenanceApi::preview(CharacterId::Crazy, &[id], 0, GraphMaintenanceMode::Clear)
            .await
            .unwrap();
    assert_eq!(preview.character(), CharacterId::Crazy);
    assert_eq!(preview.entries().len(), 1);
    assert_eq!(preview.entries()[0].source.record.id, id);
    assert_eq!(preview.entries()[0].existing_relations, vec![relation]);

    let cancel = CancellationToken::new();
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(CharacterId::Crazy, preview.clone(), false, &cancel)
            .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::ConfirmationRequired
        )),
    ));
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(CharacterId::Kind, preview.clone(), true, &cancel)
            .await,
        Err(GraphMaintenanceApiError::ForeignPreview),
    ));
    assert_eq!(kind.memories().await.unwrap().records().len(), 0);
    assert_eq!(
        crazy.memories().await.unwrap().extracted_relations().len(),
        1
    );
    cancel.cancel();
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(CharacterId::Crazy, preview.clone(), true, &cancel)
            .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Cancelled
        )),
    ));
    assert_eq!(
        crazy.memories().await.unwrap().extracted_relations().len(),
        1
    );

    let applied = GraphMaintenanceApi::apply_confirmed(
        CharacterId::Crazy,
        preview.clone(),
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!applied.applied.is_empty());
    assert!(applied.snapshot.extracted_relations().is_empty());
    assert_eq!(applied.snapshot.records().len(), 1);
    assert_eq!(
        applied.snapshot.records()[0].content,
        "Mita likes green tea"
    );
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            preview,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Write(_)
        )),
    ));

    let reindex =
        GraphMaintenanceApi::preview(CharacterId::Crazy, &[id], 0, GraphMaintenanceMode::Reindex)
            .await
            .unwrap();
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            reindex,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::BackendUnavailable
        )),
    ));
    assert!(matches!(
        GraphMaintenanceApi::preview(CharacterId::Kind, &[id], 0, GraphMaintenanceMode::Clear)
            .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::InvalidSource
        )),
    ));
    app.shutdown().await.unwrap();
    assert!(matches!(
        GraphMaintenanceApi::preview(CharacterId::Crazy, &[id], 0, GraphMaintenanceMode::Clear)
            .await,
        Err(GraphMaintenanceApiError::Unavailable),
    ));
}

#[tokio::test]
async fn stale_preview_rejects_update_without_partial_relation_writes() {
    let _single_app = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let added = owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "likes tea".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "likes coffee".into(),
                },
            ],
        )
        .await
        .unwrap();
    let ids = added
        .snapshot
        .records()
        .iter()
        .map(|x| x.id)
        .collect::<Vec<_>>();
    let plan =
        GraphMaintenanceApi::preview(CharacterId::Crazy, &ids, 0, GraphMaintenanceMode::Clear)
            .await
            .unwrap();
    let changed = owner
        .edit_memories(
            added.snapshot,
            vec![MemoryCommand::Update {
                id: ids[0],
                content: "likes cocoa".into(),
                priority: Some(MemoryPriority::High),
            }],
        )
        .await
        .unwrap();
    assert_eq!(changed.snapshot.records().len(), 2);
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            plan,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Write(_)
        )),
    ));
    let fresh = owner.memories().await.unwrap();
    assert_eq!(fresh.records().len(), 2);
    assert!(fresh.records().iter().any(|r| r.content == "likes cocoa"));
    app.shutdown().await.unwrap();
}
