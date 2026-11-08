use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use neuromita::ai_runtime::{
    AiRuntimeError, AiService, ModelActivation, ProcessTopology, PythonHostConfig,
    application::{AiApplicationConfig, AiApplicationError, AiWorkerSelection},
    backend::BackendInstaller,
    composition::{RuntimeComposition, ServiceSelection},
    overlay::{LockedOverlay, OverlayInstaller},
    runtime_status::RuntimePhase,
};
use serde_json::json;
use uuid::Uuid;

fn python() -> PathBuf {
    std::env::var_os("NEUROMITA_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        })
}

async fn python_version() -> String {
    let output = tokio::process::Command::new(python())
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let bytes = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    String::from_utf8(bytes)
        .unwrap()
        .trim()
        .strip_prefix("Python ")
        .unwrap()
        .to_owned()
}

fn plugin(service: AiService) -> &'static str {
    match service {
        AiService::Embedding => "mock_embedding",
        AiService::Reranker => "mock_reranker",
        _ => panic!("fixture only supports knowledge services"),
    }
}

async fn config(
    root: &Path,
    topology: ProcessTopology,
    services: &[AiService],
) -> AiApplicationConfig {
    let version = python_version().await;
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: root.join("python"),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let mut selections = Vec::new();
    let mut models = Vec::new();
    for &service in services {
        let plugin_id = plugin(service);
        let plugin_root = root.join("plugins").join(plugin_id);
        tokio::fs::create_dir_all(&plugin_root).await.unwrap();
        for file in ["plugin.json", "plugin.py"] {
            tokio::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("python/plugins/builtin")
                    .join(plugin_id)
                    .join(file),
                plugin_root.join(file),
            )
            .await
            .unwrap();
        }
        let overlay = LockedOverlay {
            plugin_id: plugin_id.into(),
            revision: "cycle45-test".into(),
            python_version: version.clone(),
            packages: vec![],
            actions: vec![],
        };
        let overlay_root = overlays.installed_path(&overlay).unwrap();
        tokio::fs::create_dir_all(&overlay_root).await.unwrap();
        tokio::fs::write(
            overlay_root.join(".neuromita-overlay.json"),
            serde_json::to_vec(&overlay).unwrap(),
        )
        .await
        .unwrap();
        selections.push(ServiceSelection {
            service,
            backends: vec![],
            overlays: vec![overlay],
        });
        models.push(ModelActivation {
            service,
            plugin_id: plugin_id.into(),
            settings: json!({}),
            asset_revision: None,
        });
    }
    let workers = if topology == ProcessTopology::Shared {
        vec![AiWorkerSelection {
            service: services[0],
            composition: RuntimeComposition::resolve(
                topology,
                services[0],
                &selections,
                &backends,
                &overlays,
            )
            .await
            .unwrap(),
            models,
        }]
    } else {
        let mut result = Vec::new();
        for model in models {
            let service = model.service;
            result.push(AiWorkerSelection {
                service,
                composition: RuntimeComposition::resolve(
                    topology,
                    service,
                    &selections,
                    &backends,
                    &overlays,
                )
                .await
                .unwrap(),
                models: vec![model],
            });
        }
        result
    };
    AiApplicationConfig {
        topology,
        host: PythonHostConfig {
            python_executable: python(),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: None,
            request_timeout: Duration::from_secs(8),
        },
        workers,
        voice_profiles: None,
    }
}

#[tokio::test]
async fn split_restart_reuses_pinned_selection_and_rejects_stale_ticket_without_touching_other_worker()
 {
    let root = std::env::temp_dir().join(format!("nm-restart-split-{}", Uuid::new_v4()));
    let running = config(
        &root,
        ProcessTopology::Split,
        &[AiService::Embedding, AiService::Reranker],
    )
    .await
    .activate()
    .await
    .unwrap();
    let first = running.prepare_restart(AiService::Embedding).unwrap();
    let independent = running.prepare_restart(AiService::Reranker).unwrap();
    assert_ne!(first.generation(), independent.generation());
    let fresh = running.restart(first).await.unwrap();
    assert_ne!(first.generation(), fresh);
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Embedding)
            .generation,
        Some(fresh)
    );
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Reranker)
            .generation,
        Some(independent.generation())
    );
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Embedding)
            .phase,
        RuntimePhase::ModelReady
    );
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Embedding)
            .active_model
            .unwrap()
            .plugin_id,
        "mock_embedding"
    );
    assert!(matches!(
        running.restart(first).await,
        Err(AiApplicationError::Runtime(AiRuntimeError::StaleGeneration))
    ));
    let next = running.restart(independent).await.unwrap();
    assert_ne!(next, independent.generation());
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Embedding)
            .generation,
        Some(fresh)
    );
    running.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn shared_restart_replaces_all_selected_services_and_rejects_pre_restart_ticket() {
    let root = std::env::temp_dir().join(format!("nm-restart-shared-{}", Uuid::new_v4()));
    let running = config(
        &root,
        ProcessTopology::Shared,
        &[AiService::Embedding, AiService::Reranker],
    )
    .await
    .activate()
    .await
    .unwrap();
    let before_embedding = running.prepare_restart(AiService::Embedding).unwrap();
    let before_reranker = running.prepare_restart(AiService::Reranker).unwrap();
    assert_eq!(before_embedding.generation(), before_reranker.generation());
    let fresh = running.restart(before_reranker).await.unwrap();
    assert_ne!(fresh, before_embedding.generation());
    for service in [AiService::Embedding, AiService::Reranker] {
        let state = running.status_snapshot().service(service);
        assert_eq!(state.generation, Some(fresh));
        assert_eq!(state.phase, RuntimePhase::ModelReady);
        assert_eq!(state.active_model.unwrap().plugin_id, plugin(service));
    }
    assert!(matches!(
        running.restart(before_embedding).await,
        Err(AiApplicationError::Runtime(AiRuntimeError::StaleGeneration))
    ));
    running.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn failed_candidate_keeps_old_generation_and_ticket_can_retry_only_that_selection() {
    let root = std::env::temp_dir().join(format!("nm-restart-rollback-{}", Uuid::new_v4()));
    let running = config(&root, ProcessTopology::Split, &[AiService::Embedding])
        .await
        .activate()
        .await
        .unwrap();
    let ticket = running.prepare_restart(AiService::Embedding).unwrap();
    let path = root.join("plugins/mock_embedding/plugin.py");
    let original = tokio::fs::read_to_string(&path).await.unwrap();
    let needle = "    def initialize(self, settings):\n";
    assert!(original.contains(needle));
    tokio::fs::write(&path, original.replacen(needle, "    def initialize(self, settings):\n        raise RuntimeError('cycle45 injected failure')\n", 1)).await.unwrap();
    assert!(matches!(
        running.restart(ticket).await,
        Err(AiApplicationError::Runtime(
            AiRuntimeError::ModelLoadFailed { .. }
        ))
    ));
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Embedding)
            .generation,
        Some(ticket.generation())
    );
    tokio::fs::write(&path, original).await.unwrap();
    let restored = running.restart(ticket).await.unwrap();
    assert_ne!(restored, ticket.generation());
    assert_eq!(
        running
            .status_snapshot()
            .service(AiService::Embedding)
            .phase,
        RuntimePhase::ModelReady
    );
    running.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn restart_ticket_from_other_running_application_is_rejected() {
    let root = std::env::temp_dir().join(format!("nm-restart-owner-{}", Uuid::new_v4()));
    let selected = config(&root, ProcessTopology::Split, &[AiService::Embedding]).await;
    let worker = selected.workers[0].clone();
    let host = selected.host.clone();
    let first = selected.activate().await.unwrap();
    let second = AiApplicationConfig {
        topology: ProcessTopology::Split,
        host,
        workers: vec![worker],
        voice_profiles: None,
    }
    .activate()
    .await
    .unwrap();
    let ticket = first.prepare_restart(AiService::Embedding).unwrap();
    assert!(matches!(
        second.restart(ticket).await,
        Err(AiApplicationError::ForeignRestartTicket)
    ));
    assert_eq!(
        second.status_snapshot().service(AiService::Embedding).phase,
        RuntimePhase::ModelReady
    );
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
