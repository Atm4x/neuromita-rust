use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, time::Duration};

use neuromita::{
    adapters::python_embedding::PythonEmbeddingProvider,
    ai_runtime::{
        AiService, AiSupervisor, ProcessTopology, PythonHostConfig, runtime_status::RuntimePhase,
    },
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::{
        KnowledgeRetrieval, embedding::EmbeddingSpace, semantic_memory::SemanticMemoryRetriever,
    },
    request::{InitiateRequest, RequestOutcome, RequestSource, RequestState, UserInput},
    settings::{HistoryGenerationMode, HistoryGenerationSettings, SettingsApi},
};
use serde_json::json;
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

static APP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct GeneratedCall {
    input: GenerationInput,
    response: oneshot::Sender<String>,
}

struct ObservedGenerator(mpsc::UnboundedSender<GeneratedCall>);

impl GenerationProvider for ObservedGenerator {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (response, received) = oneshot::channel();
            self.0
                .send(GeneratedCall { input, response })
                .map_err(|_| GenerationError::Transport("observation channel closed".into()))?;
            let text = received
                .await
                .map_err(|_| GenerationError::Transport("generator response dropped".into()))?;
            Ok(StructuredResponse::spoken(text))
        })
    }
}

fn application(
    sender: mpsc::UnboundedSender<GeneratedCall>,
    knowledge: KnowledgeRetrieval,
) -> ApplicationConfig {
    let template = GeminiTemplate::google(
        "test",
        "unused",
        GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let size = |n| NonZeroUsize::new(n).unwrap();
    let mut config = ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new(
            [Arc::new(ObservedGenerator(sender)) as Arc<dyn GenerationProvider>],
        )
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
    config.knowledge = knowledge;
    config
}

#[tokio::test]
async fn real_python_embedding_infer_timeout_is_terminal_and_restart_retries_same_history_source() {
    let _guard = APP_LOCK.lock().await;
    let root = std::env::temp_dir().join(format!("nm-real-python-generation-{}", Uuid::new_v4()));
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mock = manifest.join("python/plugins/builtin/mock_embedding");
    let plugin = root.join("mock_embedding");
    tokio::fs::create_dir_all(&plugin).await.unwrap();
    tokio::fs::copy(mock.join("plugin.json"), plugin.join("plugin.json"))
        .await
        .unwrap();
    let source = tokio::fs::read_to_string(mock.join("plugin.py"))
        .await
        .unwrap();
    let injection = "    def infer(self, input_data):\n        if input_data.get('purpose') == 'query':\n            from pathlib import Path\n            marker = Path(__file__).with_name('block_first_query.flag')\n            if marker.exists():\n                marker.unlink()\n                import time\n                time.sleep(9)\n";
    assert!(source.contains("    def infer(self, input_data):\n"));
    let source = source.replacen("    def infer(self, input_data):\n", injection, 1);
    tokio::fs::write(plugin.join("plugin.py"), source)
        .await
        .unwrap();
    tokio::fs::write(plugin.join("block_first_query.flag"), "once")
        .await
        .unwrap();

    let supervisor = Arc::new(AiSupervisor::new(
        ProcessTopology::Split,
        PythonHostConfig {
            python_executable: std::env::var_os("NEUROMITA_TEST_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| "python3".into()),
            sdk_root: manifest.join("python"),
            plugins_directory: root.clone(),
            models_directory: None,
            request_timeout: Duration::from_secs(4),
        },
    ));
    let generation = supervisor.start(AiService::Embedding).await.unwrap();
    supervisor
        .initialize(AiService::Embedding, "mock_embedding", json!({}))
        .await
        .unwrap();
    let space = EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap();
    let provider = Arc::new(PythonEmbeddingProvider::new(Arc::clone(&supervisor), space));
    let retriever = SemanticMemoryRetriever::new(provider, 16).unwrap();
    let (sender, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(application(
        sender,
        KnowledgeRetrieval::semantic(Arc::new(retriever)),
    ))
    .await
    .unwrap();

    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Архив: чай с ромашкой".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Текущая беседа".into()).unwrap(),
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

    let failed = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "чай",
        RequestSource::Presenter,
    ))
    .unwrap();
    let result = timeout(Duration::from_secs(14), failed.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&result.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(error))
                if error.to_string().contains("timed out")
        ),
        "{:?}",
        result.state
    );
    assert!(calls.try_recv().is_err(), "timeout reached LLM provider");
    let status = supervisor.status_snapshot().service(AiService::Embedding);
    assert_eq!(status.phase, RuntimePhase::Failed);
    assert_eq!(status.generation, Some(generation));
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("timed out")
    );
    assert!(
        !tokio::fs::try_exists(plugin.join("block_first_query.flag"))
            .await
            .unwrap()
    );

    supervisor.stop(AiService::Embedding).await.unwrap();
    let restarted = supervisor.start(AiService::Embedding).await.unwrap();
    assert_ne!(generation, restarted);
    supervisor
        .initialize(AiService::Embedding, "mock_embedding", json!({}))
        .await
        .unwrap();
    assert_eq!(
        supervisor
            .status_snapshot()
            .service(AiService::Embedding)
            .phase,
        RuntimePhase::ModelReady
    );

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
    assert!(evidence.contains("Архив: чай с ромашкой"), "{evidence}");
    observed.response.send("Восстановлено".into()).unwrap();
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
    supervisor.stop(AiService::Embedding).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

async fn prepared_worker(
    root: &std::path::Path,
    service: AiService,
    plugin_id: &str,
    python_version: &str,
) -> neuromita::ai_runtime::application::AiWorkerSelection {
    use neuromita::ai_runtime::{
        ModelActivation,
        application::AiWorkerSelection,
        backend::BackendInstaller,
        composition::{RuntimeComposition, ServiceSelection},
        overlay::{LockedOverlay, OverlayInstaller},
    };

    let overlay = LockedOverlay {
        plugin_id: plugin_id.into(),
        revision: "test".into(),
        python_version: python_version.into(),
        packages: Vec::new(),
        actions: Vec::new(),
    };
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let directory = overlays.installed_path(&overlay).unwrap();
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(
        directory.join(".neuromita-overlay.json"),
        serde_json::to_vec(&overlay).unwrap(),
    )
    .await
    .unwrap();
    let composition = RuntimeComposition::resolve(
        ProcessTopology::Split,
        service,
        &[ServiceSelection {
            service,
            backends: Vec::new(),
            overlays: vec![overlay],
        }],
        &backends,
        &overlays,
    )
    .await
    .unwrap();
    AiWorkerSelection {
        service,
        composition,
        models: vec![ModelActivation {
            service,
            plugin_id: plugin_id.into(),
            settings: json!({}),
            asset_revision: None,
        }],
    }
}

#[tokio::test]
async fn application_ai_knowledge_composes_actual_python_embedding_and_reranker_for_history() {
    let _guard = APP_LOCK.lock().await;
    use neuromita::{
        ai_runtime::application::AiApplicationConfig,
        app::{AiKnowledgeSelection, AiRerankSelection},
        knowledge::{reranking::RerankPolicy, retrieval_fusion::FusionMode},
    };

    let root = std::env::temp_dir().join(format!("nm-python-composed-rag-{}", Uuid::new_v4()));
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let python = std::env::var_os("NEUROMITA_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| "python3".into());
    let output = tokio::process::Command::new(&python)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let version = String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("Python ")
        .unwrap()
        .to_owned();
    let workers = vec![
        prepared_worker(&root, AiService::Embedding, "mock_embedding", &version).await,
        prepared_worker(&root, AiService::Reranker, "mock_reranker", &version).await,
    ];
    let (sender, mut calls) = mpsc::unbounded_channel();
    let mut config = application(sender, KnowledgeRetrieval::lexical());
    config.ai_runtime = Some(AiApplicationConfig {
        topology: ProcessTopology::Split,
        host: PythonHostConfig {
            python_executable: python,
            sdk_root: manifest.join("python"),
            plugins_directory: manifest.join("python/plugins/builtin"),
            models_directory: None,
            request_timeout: Duration::from_secs(8),
        },
        workers,
        voice_profiles: None,
    });
    config.ai_knowledge = Some(AiKnowledgeSelection {
        space: EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
        cache_entries: 32,
        fusion: FusionMode::VectorOnly,
        reranker: Some(AiRerankSelection {
            model_id: "rerank-fixture".into(),
            protocol: "fixture-rerank-v1".into(),
            policy: RerankPolicy {
                alpha: 0.75,
                limit: 8,
            },
        }),
    });
    let app = ApplicationLifecycle::start(config).await.unwrap();
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
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "чай",
        RequestSource::Presenter,
    ))
    .unwrap();
    let observed = timeout(Duration::from_secs(12), calls.recv())
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
    observed.response.send("Готово".into()).unwrap();
    let result = timeout(Duration::from_secs(8), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn real_python_reranker_infer_timeout_is_terminal_and_restart_retries_same_history_source() {
    use neuromita::{
        adapters::python_reranker::PythonReranker, knowledge::reranking::RerankPolicy,
    };

    let _guard = APP_LOCK.lock().await;
    let root = std::env::temp_dir().join(format!("nm-real-python-rerank-{}", Uuid::new_v4()));
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for plugin_id in ["mock_embedding", "mock_reranker"] {
        let source_dir = manifest.join("python/plugins/builtin").join(plugin_id);
        let target = root.join(plugin_id);
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::copy(source_dir.join("plugin.json"), target.join("plugin.json"))
            .await
            .unwrap();
        let mut source = tokio::fs::read_to_string(source_dir.join("plugin.py"))
            .await
            .unwrap();
        if plugin_id == "mock_reranker" {
            let declaration = "    def infer(self, input_data):\n";
            let injection = "    def infer(self, input_data):\n        from pathlib import Path\n        marker = Path(__file__).with_name('block_first_rerank.flag')\n        if marker.exists():\n            marker.unlink()\n            import time\n            time.sleep(9)\n";
            assert!(source.contains(declaration));
            source = source.replacen(declaration, injection, 1);
            tokio::fs::write(target.join("block_first_rerank.flag"), "once")
                .await
                .unwrap();
        }
        tokio::fs::write(target.join("plugin.py"), source)
            .await
            .unwrap();
    }
    let supervisor = Arc::new(AiSupervisor::new(
        ProcessTopology::Split,
        PythonHostConfig {
            python_executable: std::env::var_os("NEUROMITA_TEST_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| "python3".into()),
            sdk_root: manifest.join("python"),
            plugins_directory: root.clone(),
            models_directory: None,
            request_timeout: Duration::from_secs(4),
        },
    ));
    supervisor.start(AiService::Embedding).await.unwrap();
    supervisor
        .initialize(AiService::Embedding, "mock_embedding", json!({}))
        .await
        .unwrap();
    let old_generation = supervisor.start(AiService::Reranker).await.unwrap();
    supervisor
        .initialize(AiService::Reranker, "mock_reranker", json!({}))
        .await
        .unwrap();
    let provider = Arc::new(PythonEmbeddingProvider::new(
        Arc::clone(&supervisor),
        EmbeddingSpace::new("python", "fixture", "fixture-v1", 3).unwrap(),
    ));
    let reranker = Arc::new(
        PythonReranker::new(
            Arc::clone(&supervisor),
            "rerank-fixture".into(),
            "fixture-rerank-v1".into(),
        )
        .unwrap(),
    );
    let retriever = SemanticMemoryRetriever::new(provider, 16)
        .unwrap()
        .with_reranker(
            reranker,
            RerankPolicy {
                alpha: 0.75,
                limit: 8,
            },
        )
        .unwrap();
    let (sender, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(application(
        sender,
        KnowledgeRetrieval::semantic(Arc::new(retriever)),
    ))
    .await
    .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Архивный чай и лимон".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Текущая фраза".into()).unwrap(),
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

    let failed = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "чай",
        RequestSource::Presenter,
    ))
    .unwrap();
    let result = timeout(Duration::from_secs(14), failed.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            &result.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(error))
                if error.to_string().contains("timed out")
        ),
        "{:?}",
        result.state
    );
    assert!(
        calls.try_recv().is_err(),
        "failed Python reranker invoked LLM"
    );
    let status = supervisor.status_snapshot().service(AiService::Reranker);
    assert_eq!(status.phase, RuntimePhase::Failed);
    assert_eq!(status.generation, Some(old_generation));
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("timed out")
    );
    assert_eq!(
        supervisor
            .status_snapshot()
            .service(AiService::Embedding)
            .phase,
        RuntimePhase::ModelReady
    );
    supervisor.stop(AiService::Reranker).await.unwrap();
    let next_generation = supervisor.start(AiService::Reranker).await.unwrap();
    assert_ne!(old_generation, next_generation);
    supervisor
        .initialize(AiService::Reranker, "mock_reranker", json!({}))
        .await
        .unwrap();

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
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(evidence.contains("Архивный чай и лимон"), "{evidence}");
    observed.response.send("Работает".into()).unwrap();
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
    supervisor.stop(AiService::Reranker).await.unwrap();
    supervisor.stop(AiService::Embedding).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
