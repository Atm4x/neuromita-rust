use std::{path::PathBuf, time::Duration};

use neuromita::{
    ai_runtime::{
        AiRuntimeError, AiService, ModelActivation, ProcessTopology, PythonHostConfig,
        application::{AiApplicationConfig, AiApplicationError, AiWorkerSelection},
        backend::BackendInstaller,
        composition::{RuntimeComposition, ServiceSelection},
        overlay::{LockedOverlay, OverlayInstaller},
        runtime_status::RuntimePhase,
    },
    knowledge::embedding::{EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace},
};
use serde_json::json;
use uuid::Uuid;

static APPLICATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    root: PathBuf,
    config: AiApplicationConfig,
}

impl Fixture {
    async fn new(topology: ProcessTopology) -> Self {
        let root = std::env::temp_dir().join(format!("nm-ai-owner-recovery-{}", Uuid::new_v4()));
        let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let source = sdk.join("python/plugins/builtin/mock_embedding");
        let target = root.join("plugins/mock_embedding");
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::copy(source.join("plugin.json"), target.join("plugin.json"))
            .await
            .unwrap();
        let script = tokio::fs::read_to_string(source.join("plugin.py"))
            .await
            .unwrap();
        let inject = "    def infer(self, input_data):\n        if input_data.get('purpose') == 'query':\n            from pathlib import Path\n            marker = Path(__file__).with_name('crash.once')\n            if marker.exists():\n                marker.unlink()\n                import os\n                os._exit(73)\n";
        assert!(script.contains("    def infer(self, input_data):\n"));
        tokio::fs::write(
            target.join("plugin.py"),
            script.replacen("    def infer(self, input_data):\n", inject, 1),
        )
        .await
        .unwrap();
        tokio::fs::write(target.join("crash.once"), "1")
            .await
            .unwrap();
        let reranker_source = sdk.join("python/plugins/builtin/mock_reranker");
        let reranker_target = root.join("plugins/mock_reranker");
        tokio::fs::create_dir_all(&reranker_target).await.unwrap();
        for name in ["plugin.json", "plugin.py"] {
            tokio::fs::copy(reranker_source.join(name), reranker_target.join(name))
                .await
                .unwrap();
        }
        let python = std::env::var_os("NEUROMITA_TEST_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| "python3".into());
        let version = tokio::process::Command::new(&python)
            .arg("--version")
            .output()
            .await
            .unwrap();
        let output = if version.stdout.is_empty() {
            &version.stderr
        } else {
            &version.stdout
        };
        let python_version = String::from_utf8_lossy(output)
            .trim()
            .strip_prefix("Python ")
            .unwrap()
            .to_string();
        let overlays = OverlayInstaller {
            uv_executable: root.join("uv"),
            python_executable: python.clone(),
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let backends = BackendInstaller {
            uv_executable: root.join("uv"),
            python_executable: python.clone(),
            backends_directory: root.join("backends"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let mut selections = Vec::new();
        for (service, plugin) in [
            (AiService::Embedding, "mock_embedding"),
            (AiService::Reranker, "mock_reranker"),
        ] {
            let overlay = LockedOverlay {
                plugin_id: plugin.into(),
                revision: "recovery-fixture".into(),
                python_version: python_version.clone(),
                packages: vec![],
                actions: vec![],
            };
            let path = overlays.installed_path(&overlay).unwrap();
            tokio::fs::create_dir_all(&path).await.unwrap();
            tokio::fs::write(
                path.join(".neuromita-overlay.json"),
                serde_json::to_vec(&overlay).unwrap(),
            )
            .await
            .unwrap();
            selections.push(ServiceSelection {
                service,
                backends: vec![],
                overlays: vec![overlay],
            });
        }
        let mut workers = Vec::new();
        match topology {
            ProcessTopology::Split => {
                for selected in &selections {
                    let service = selected.service;
                    let composition = RuntimeComposition::resolve(
                        topology,
                        service,
                        std::slice::from_ref(selected),
                        &backends,
                        &overlays,
                    )
                    .await
                    .unwrap();
                    workers.push(AiWorkerSelection {
                        service,
                        composition,
                        models: vec![ModelActivation {
                            service,
                            plugin_id: service_plugin(service).into(),
                            settings: json!({}),
                            asset_revision: None,
                        }],
                    });
                }
            }
            ProcessTopology::Shared => {
                let composition = RuntimeComposition::resolve(
                    topology,
                    AiService::Embedding,
                    &selections,
                    &backends,
                    &overlays,
                )
                .await
                .unwrap();
                workers.push(AiWorkerSelection {
                    service: AiService::Embedding,
                    composition,
                    models: [AiService::Embedding, AiService::Reranker]
                        .into_iter()
                        .map(|service| ModelActivation {
                            service,
                            plugin_id: service_plugin(service).into(),
                            settings: json!({}),
                            asset_revision: None,
                        })
                        .collect(),
                });
            }
        }
        Self {
            config: AiApplicationConfig {
                topology,
                host: PythonHostConfig {
                    python_executable: python,
                    sdk_root: sdk.join("python"),
                    plugins_directory: root.join("plugins"),
                    models_directory: None,
                    request_timeout: Duration::from_secs(5),
                },
                workers,
                voice_profiles: None,
            },
            root,
        }
    }
}

fn service_plugin(service: AiService) -> &'static str {
    match service {
        AiService::Embedding => "mock_embedding",
        AiService::Reranker => "mock_reranker",
        _ => unreachable!(),
    }
}

async fn recovery_is_version_checked_and_topology_safe(topology: ProcessTopology) {
    let fixture = Fixture::new(topology).await;
    let app = Box::pin(fixture.config.activate()).await.unwrap();
    let embedding = app
        .embedding_provider(EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap())
        .unwrap();
    let recovery = app.recovery_handle();
    let initial = app.status_snapshot().service(AiService::Embedding);
    let other = app.status_snapshot().service(AiService::Reranker);
    assert_eq!(initial.phase, RuntimePhase::ModelReady);
    assert_eq!(other.phase, RuntimePhase::ModelReady);
    let generation = initial.generation.unwrap();
    assert!(matches!(
        app.restart_failed_worker(AiService::Embedding, generation)
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::WorkerNotFailed))
    ));
    assert!(matches!(
        app.restart_failed_worker(AiService::Asr, generation).await,
        Err(AiApplicationError::Invalid(_))
    ));
    let failure = embedding
        .embed(EmbeddingPurpose::Query, "чай".into())
        .await
        .unwrap_err();
    assert!(failure.to_string().contains("worker closed stdout"));
    let failed = app.refresh_status().await.service(AiService::Embedding);
    assert_eq!(failed.phase, RuntimePhase::Failed);
    assert_eq!(failed.generation, Some(generation));
    assert!(failed.last_error.unwrap().contains("worker closed stdout"));
    assert!(matches!(
        app.restart_failed_worker(AiService::Embedding, Uuid::new_v4())
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::StaleGeneration))
    ));
    let replacement = app
        .restart_failed_worker(AiService::Embedding, generation)
        .await
        .unwrap();
    assert_ne!(replacement, generation);
    let new_embedding = app.status_snapshot().service(AiService::Embedding);
    let new_reranker = app.status_snapshot().service(AiService::Reranker);
    assert_eq!(new_embedding.phase, RuntimePhase::ModelReady);
    assert_eq!(new_embedding.generation, Some(replacement));
    assert_eq!(
        new_embedding.active_model.unwrap().plugin_id,
        "mock_embedding"
    );
    match topology {
        ProcessTopology::Split => assert_eq!(new_reranker.generation, other.generation),
        ProcessTopology::Shared => assert_eq!(new_reranker.generation, Some(replacement)),
    }
    assert!(matches!(
        app.restart_failed_worker(AiService::Embedding, generation)
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::StaleGeneration))
    ));
    assert!(matches!(
        app.restart_failed_worker(AiService::Embedding, replacement)
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::WorkerNotFailed))
    ));
    embedding
        .embed(EmbeddingPurpose::Query, "чай".into())
        .await
        .unwrap();
    app.shutdown().await.unwrap();
    assert!(matches!(
        recovery
            .restart_failed_worker(AiService::Embedding, replacement)
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::NotRunning))
    ));
    tokio::fs::remove_dir_all(fixture.root).await.unwrap();
}

fn run_recovery_test(topology: ProcessTopology) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(recovery_is_version_checked_and_topology_safe(topology));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn split_restart_only_replaces_failed_embedding_generation() {
    run_recovery_test(ProcessTopology::Split);
}

#[test]
fn shared_restart_recovers_all_selected_services_atomically() {
    run_recovery_test(ProcessTopology::Shared);
}

#[test]
fn application_lifecycle_exposes_only_authorized_failed_worker_recovery() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let _guard = APPLICATION_LOCK.lock().await;
                    use neuromita::{
                        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
                        generation::{
                            GenerationPreset, GenerationProvider, ProviderTemplate,
                            ResponseGenerator,
                            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
                        },
                    };
                    use std::{num::NonZeroUsize, sync::Arc};
                    let fixture = Fixture::new(ProcessTopology::Split).await;
                    let template = GeminiTemplate::google(
                        "fixture",
                        "unused",
                        GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
                    )
                    .unwrap();
                    let size = |value| NonZeroUsize::new(value).unwrap();
                    let mut config = ApplicationConfig::minimal(
                        GenerationPreset::new(ProviderTemplate::Gemini(template)),
                        ResponseGenerator::new([
                            Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
                        ])
                        .unwrap(),
                        RuntimeLimits {
                            request_queue: size(4),
                            discovery_buffer: size(8),
                            character_commands: size(16),
                            presenter_updates: size(8),
                            retained_finished: size(8),
                        },
                        "127.0.0.1:0".parse().unwrap(),
                    );
                    config.ai_runtime = Some(fixture.config);
                    let app = Box::pin(ApplicationLifecycle::start(config)).await.unwrap();
                    let status = app.presenter().ai_runtime.refresh_status().await;
                    let generation = status.service(AiService::Embedding).generation.unwrap();
                    assert!(matches!(
                        app.restart_failed_ai_worker(AiService::Embedding, generation)
                            .await,
                        Err(AiApplicationError::Runtime(AiRuntimeError::WorkerNotFailed))
                    ));
                    assert!(matches!(
                        app.restart_failed_ai_worker(AiService::Asr, generation)
                            .await,
                        Err(AiApplicationError::Invalid(_))
                    ));
                    app.shutdown().await.unwrap();
                    tokio::fs::remove_dir_all(fixture.root).await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

struct ObservedGeneration(tokio::sync::mpsc::UnboundedSender<ObservedCall>);

struct ObservedCall {
    input: neuromita::generation::GenerationInput,
    reply: tokio::sync::oneshot::Sender<String>,
}

impl neuromita::generation::GenerationProvider for ObservedGeneration {
    fn kind(&self) -> neuromita::generation::ProviderKind {
        neuromita::generation::ProviderKind::Gemini
    }

    fn generate(
        &self,
        input: neuromita::generation::GenerationInput,
        _: neuromita::generation::ProviderTemplate,
    ) -> neuromita::generation::ProviderFuture<'_> {
        Box::pin(async move {
            use neuromita::generation::{GenerationError, StructuredResponse};
            let (reply, received) = tokio::sync::oneshot::channel();
            self.0
                .send(ObservedCall { input, reply })
                .map_err(|_| GenerationError::Transport("observer disconnected".into()))?;
            let content = received
                .await
                .map_err(|_| GenerationError::Transport("observer reply dropped".into()))?;
            Ok(StructuredResponse::spoken(content))
        })
    }
}

async fn request_fails_then_owner_recovers_canonical_history() {
    use neuromita::{
        api::ChatApi,
        app::{AiKnowledgeSelection, ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        character::CharacterId,
        generation::{
            GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiTemplate, GenerationSettings},
        },
        knowledge::{embedding::EmbeddingSpace, retrieval_fusion::FusionMode},
        request::{InitiateRequest, RequestOutcome, RequestSource, RequestState, UserInput},
        settings::{HistoryGenerationMode, HistoryGenerationSettings, SettingsApi},
    };
    use std::{num::NonZeroUsize, sync::Arc};
    use tokio::{sync::mpsc, time::timeout};

    let _guard = APPLICATION_LOCK.lock().await;
    let fixture = Fixture::new(ProcessTopology::Split).await;
    let template = GeminiTemplate::google(
        "fixture",
        "unused",
        GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let (sender, mut calls) = mpsc::unbounded_channel();
    let size = |value| NonZeroUsize::new(value).unwrap();
    let mut config = ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new([
            Arc::new(ObservedGeneration(sender)) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        RuntimeLimits {
            request_queue: size(4),
            discovery_buffer: size(8),
            character_commands: size(16),
            presenter_updates: size(8),
            retained_finished: size(8),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.ai_runtime = Some(fixture.config);
    config.ai_knowledge = Some(AiKnowledgeSelection {
        space: EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
        cache_entries: 16,
        fusion: FusionMode::VectorOnly,
        reranker: None,
    });
    let app = Box::pin(ApplicationLifecycle::start(config)).await.unwrap();
    let first_generation = app
        .presenter()
        .ai_runtime
        .refresh_status()
        .await
        .service(AiService::Embedding)
        .generation
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Архивный чай с ромашкой".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Текущая тема".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let first = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "чай",
        RequestSource::Presenter,
    ))
    .unwrap();
    let result = timeout(Duration::from_secs(12), first.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &result.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(error))
            if error.to_string().contains("worker closed stdout")
    ));
    assert!(calls.try_recv().is_err(), "failed worker reached LLM");
    let failed = app
        .presenter()
        .ai_runtime
        .refresh_status()
        .await
        .service(AiService::Embedding);
    assert_eq!(failed.phase, RuntimePhase::Failed);
    assert_eq!(failed.generation, Some(first_generation));
    assert!(failed.last_error.unwrap().contains("worker closed stdout"));
    assert!(matches!(
        app.restart_failed_ai_worker(AiService::Embedding, Uuid::new_v4())
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::StaleGeneration))
    ));
    let restored = app
        .restart_failed_ai_worker(AiService::Embedding, first_generation)
        .await
        .unwrap();
    assert_ne!(restored, first_generation);
    assert!(matches!(
        app.restart_failed_ai_worker(AiService::Embedding, first_generation)
            .await,
        Err(AiApplicationError::Runtime(AiRuntimeError::StaleGeneration))
    ));
    let ready = app
        .presenter()
        .ai_runtime
        .refresh_status()
        .await
        .service(AiService::Embedding);
    assert_eq!(ready.phase, RuntimePhase::ModelReady);
    assert_eq!(ready.generation, Some(restored));

    let retry = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "чай",
        RequestSource::Presenter,
    ))
    .unwrap();
    let observed = timeout(Duration::from_secs(8), calls.recv())
        .await
        .unwrap()
        .unwrap();
    let evidence = observed.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(
        evidence.contains("[RETRIEVED TIMELINE EVIDENCE]"),
        "{evidence}"
    );
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(evidence.contains("Архивный чай с ромашкой"), "{evidence}");
    observed.reply.send("Восстановлено".into()).unwrap();
    let result = timeout(Duration::from_secs(8), retry.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(fixture.root).await.unwrap();
}

#[test]
fn public_lifecycle_recovers_actual_failed_request_with_original_archived_event() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(request_fails_then_owner_recovers_canonical_history());
        })
        .unwrap()
        .join()
        .unwrap();
}
