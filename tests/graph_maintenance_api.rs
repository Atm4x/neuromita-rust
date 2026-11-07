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
        graph::{GraphError, GraphExtractionFuture, GraphExtractor},
        maintenance::{GraphMaintenanceError, GraphMaintenanceMode},
    },
    memory::{MemoryCommand, MemoryPriority, MemoryRecord, MemoryRelation},
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
    assert_eq!(GraphMaintenanceApi::reindex_available().unwrap(), false);
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

struct ConfiguredExtractor {
    fail_on: Option<&'static str>,
}

impl GraphExtractor for ConfiguredExtractor {
    fn extract<'a>(&'a self, source: &'a MemoryRecord) -> GraphExtractionFuture<'a> {
        Box::pin(async move {
            if self
                .fail_on
                .is_some_and(|term| source.content.contains(term))
            {
                return Err(GraphError::Provider("selected worker failed".into()));
            }
            let object = if source.content.contains("coffee") {
                "coffee"
            } else {
                "tea"
            };
            Ok(vec![MemoryRelation {
                subject: "Mita".into(),
                predicate: "likes".into(),
                object: object.into(),
            }])
        })
    }
}

#[tokio::test]
async fn configured_reindex_runs_from_live_api_and_preserves_source_versions() {
    let _single_app = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(
        config().with_graph_extractor(Arc::new(ConfiguredExtractor { fail_on: None })),
    )
    .await
    .unwrap();
    assert!(GraphMaintenanceApi::reindex_available().unwrap());
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let kind = CharacterApi::get(CharacterId::Kind).unwrap();
    let added = crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "Mita likes tea".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Mita likes coffee".into(),
                },
            ],
        )
        .await
        .unwrap();
    let ids = added
        .snapshot
        .records()
        .iter()
        .map(|r| r.id)
        .collect::<Vec<_>>();
    let preview =
        GraphMaintenanceApi::preview(CharacterId::Crazy, &ids, 0, GraphMaintenanceMode::Reindex)
            .await
            .unwrap();
    let cancel = CancellationToken::new();
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(CharacterId::Crazy, preview.clone(), false, &cancel)
            .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::ConfirmationRequired
        ))
    ));
    assert!(
        crazy
            .memories()
            .await
            .unwrap()
            .extracted_relations()
            .is_empty()
    );
    let reindexed =
        GraphMaintenanceApi::apply_confirmed(CharacterId::Crazy, preview, true, &cancel)
            .await
            .unwrap();
    assert_eq!(reindexed.snapshot.extracted_relations().len(), 2);
    for relation in reindexed.snapshot.extracted_relations() {
        assert!(ids.contains(&relation.source));
        assert_eq!(
            relation.source_revision,
            reindexed.snapshot.source_revision(relation.source).unwrap()
        );
        assert_eq!(relation.relation.predicate, "likes");
    }
    assert_eq!(reindexed.snapshot.records().len(), 2);
    let duplicate =
        GraphMaintenanceApi::preview(CharacterId::Crazy, &ids, 0, GraphMaintenanceMode::Reindex)
            .await
            .unwrap();
    let repeated = GraphMaintenanceApi::apply_confirmed(
        CharacterId::Crazy,
        duplicate,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        repeated.applied.is_empty(),
        "unchanged extracted facts must be idempotent"
    );
    assert_eq!(repeated.snapshot.extracted_relations().len(), 2);
    assert!(kind.memories().await.unwrap().records().is_empty());
    assert!(
        kind.memories()
            .await
            .unwrap()
            .extracted_relations()
            .is_empty()
    );
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Kind,
            GraphMaintenanceApi::preview(
                CharacterId::Crazy,
                &ids,
                0,
                GraphMaintenanceMode::Reindex
            )
            .await
            .unwrap(),
            true,
            &cancel
        )
        .await,
        Err(GraphMaintenanceApiError::ForeignPreview)
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_extractor_failure_or_stale_preview_cannot_partially_commit_batch() {
    let _single_app = APPLICATION.lock().await;
    let app =
        ApplicationLifecycle::start(config().with_graph_extractor(Arc::new(ConfiguredExtractor {
            fail_on: Some("coffee"),
        })))
        .await
        .unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let added = crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            vec![
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Mita likes tea".into(),
                },
                MemoryCommand::Add {
                    priority: MemoryPriority::Normal,
                    content: "Mita likes coffee".into(),
                },
            ],
        )
        .await
        .unwrap();
    let ids = added
        .snapshot
        .records()
        .iter()
        .map(|r| r.id)
        .collect::<Vec<_>>();
    let failed_plan =
        GraphMaintenanceApi::preview(CharacterId::Crazy, &ids, 0, GraphMaintenanceMode::Reindex)
            .await
            .unwrap();
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            failed_plan,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Extraction(GraphError::Provider(_))
        ))
    ));
    assert!(
        crazy
            .memories()
            .await
            .unwrap()
            .extracted_relations()
            .is_empty()
    );
    let stale = GraphMaintenanceApi::preview(
        CharacterId::Crazy,
        &[ids[0]],
        0,
        GraphMaintenanceMode::Reindex,
    )
    .await
    .unwrap();
    let updated = crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            vec![MemoryCommand::Update {
                id: ids[0],
                content: "Mita likes black tea".into(),
                priority: None,
            }],
        )
        .await
        .unwrap();
    assert_eq!(updated.snapshot.records().len(), 2);
    assert!(matches!(
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            stale,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Write(_)
        ))
    ));
    let current = crazy.memories().await.unwrap();
    assert_eq!(current.records().len(), 2);
    assert!(current.extracted_relations().is_empty());
    app.shutdown().await.unwrap();
}

struct PausedExtractor {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}

impl GraphExtractor for PausedExtractor {
    fn extract<'a>(&'a self, _: &'a MemoryRecord) -> GraphExtractionFuture<'a> {
        Box::pin(async move {
            self.entered.notify_one();
            self.resume.notified().await;
            Ok(vec![MemoryRelation {
                subject: "Mita".into(),
                predicate: "likes".into(),
                object: "tea".into(),
            }])
        })
    }
}

#[tokio::test]
async fn configured_reindex_revalidates_after_async_extraction_and_honors_cancellation() {
    let _single_app = APPLICATION.lock().await;
    let extractor = Arc::new(PausedExtractor {
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let app = ApplicationLifecycle::start(config().with_graph_extractor(extractor.clone()))
        .await
        .unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let added = crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Mita likes tea".into(),
            }],
        )
        .await
        .unwrap();
    let source = added.snapshot.records()[0].id;
    let stale = GraphMaintenanceApi::preview(
        CharacterId::Crazy,
        &[source],
        0,
        GraphMaintenanceMode::Reindex,
    )
    .await
    .unwrap();
    let stale_task = tokio::spawn(async move {
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            stale,
            true,
            &CancellationToken::new(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), extractor.entered.notified())
        .await
        .expect("extractor was not invoked");
    let changed = crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            vec![MemoryCommand::Update {
                id: source,
                content: "Mita likes cocoa".into(),
                priority: None,
            }],
        )
        .await
        .unwrap();
    assert_eq!(changed.snapshot.records()[0].content, "Mita likes cocoa");
    extractor.resume.notify_one();
    assert!(matches!(
        stale_task.await.unwrap(),
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Write(_)
        ))
    ));
    assert!(
        crazy
            .memories()
            .await
            .unwrap()
            .extracted_relations()
            .is_empty()
    );

    let preview = GraphMaintenanceApi::preview(
        CharacterId::Crazy,
        &[source],
        0,
        GraphMaintenanceMode::Reindex,
    )
    .await
    .unwrap();
    let cancel = CancellationToken::new();
    let cancelled = cancel.clone();
    let task = tokio::spawn(async move {
        GraphMaintenanceApi::apply_confirmed(CharacterId::Crazy, preview, true, &cancelled).await
    });
    tokio::time::timeout(Duration::from_secs(5), extractor.entered.notified())
        .await
        .expect("extractor was not invoked on retry");
    cancel.cancel();
    assert!(matches!(
        task.await.unwrap(),
        Err(GraphMaintenanceApiError::Maintenance(
            GraphMaintenanceError::Cancelled
        ))
    ));
    assert!(
        crazy
            .memories()
            .await
            .unwrap()
            .extracted_relations()
            .is_empty()
    );
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn live_configured_reindex_survives_sqlite_restart_and_default_backend_is_not_inferred() {
    let _single_app = APPLICATION.lock().await;
    let root = std::env::temp_dir().join(format!(
        "neuromita-live-graph-reindex-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let make_config = |with_backend: bool| {
        let mut application = config();
        application.timeline = neuromita::timeline::TimelineConfig::Persistent {
            database: root.join("timeline.sqlite3"),
        };
        if with_backend {
            application =
                application.with_graph_extractor(Arc::new(ConfiguredExtractor { fail_on: None }));
        }
        application
    };
    let id;
    {
        let app = ApplicationLifecycle::start(make_config(true))
            .await
            .unwrap();
        assert!(GraphMaintenanceApi::reindex_available().unwrap());
        let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
        let added = owner
            .edit_memories(
                owner.memories().await.unwrap(),
                vec![MemoryCommand::Add {
                    priority: MemoryPriority::High,
                    content: "Mita likes tea".into(),
                }],
            )
            .await
            .unwrap();
        id = added.snapshot.records()[0].id;
        let preview = GraphMaintenanceApi::preview(
            CharacterId::Crazy,
            &[id],
            0,
            GraphMaintenanceMode::Reindex,
        )
        .await
        .unwrap();
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            preview,
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        app.shutdown().await.unwrap();
    }
    {
        let app = ApplicationLifecycle::start(make_config(false))
            .await
            .unwrap();
        assert!(!GraphMaintenanceApi::reindex_available().unwrap());
        let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
        let snapshot = owner.memories().await.unwrap();
        assert_eq!(snapshot.records().len(), 1);
        assert_eq!(snapshot.records()[0].id, id);
        assert_eq!(snapshot.extracted_relations().len(), 1);
        let clear =
            GraphMaintenanceApi::preview(CharacterId::Crazy, &[id], 0, GraphMaintenanceMode::Clear)
                .await
                .unwrap();
        GraphMaintenanceApi::apply_confirmed(
            CharacterId::Crazy,
            clear,
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let preview = GraphMaintenanceApi::preview(
            CharacterId::Crazy,
            &[id],
            0,
            GraphMaintenanceMode::Reindex,
        )
        .await
        .unwrap();
        assert!(matches!(
            GraphMaintenanceApi::apply_confirmed(
                CharacterId::Crazy,
                preview,
                true,
                &CancellationToken::new()
            )
            .await,
            Err(GraphMaintenanceApiError::Maintenance(
                GraphMaintenanceError::BackendUnavailable
            ))
        ));
        app.shutdown().await.unwrap();
    }
    {
        let app = ApplicationLifecycle::start(make_config(false))
            .await
            .unwrap();
        let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
        let snapshot = owner.memories().await.unwrap();
        assert_eq!(snapshot.records().len(), 1);
        assert_eq!(snapshot.records()[0].id, id);
        assert!(snapshot.extracted_relations().is_empty());
        app.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}
