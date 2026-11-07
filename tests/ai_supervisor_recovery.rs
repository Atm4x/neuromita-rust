use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use neuromita::ai_runtime::{
    AiRuntimeError, AiService, AiSupervisor, ModelActivation, ProcessTopology, PythonHostConfig,
    application::{AiApplicationConfig, AiWorkerSelection},
    backend::BackendInstaller,
    composition::{RuntimeComposition, ServiceSelection},
    overlay::{LockedOverlay, OverlayInstaller},
    runtime_status::RuntimePhase,
};
use neuromita::{settings::Volume, voice::VoiceOptions};
use serde_json::json;
use uuid::Uuid;

fn test_python() -> PathBuf {
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

async fn fixture(
    root: &Path,
    topology: ProcessTopology,
    services: &[AiService],
) -> (AiSupervisor, RuntimeComposition, Vec<ModelActivation>) {
    let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
    let plugins = root.join("plugins");
    let source = sdk.join("plugins/builtin/mock_tts");
    let tts = plugins.join("mock_tts");
    tokio::fs::create_dir_all(&tts).await.unwrap();
    tokio::fs::copy(source.join("plugin.json"), tts.join("plugin.json"))
        .await
        .unwrap();
    let code = tokio::fs::read_to_string(source.join("plugin.py"))
        .await
        .unwrap()
        .replace("    def infer(self, input_data):\n", "    def infer(self, input_data):\n        if input_data.get('crash') or input_data.get('text') == 'CRASH':\n            import os\n            os._exit(43)\n");
    tokio::fs::write(tts.join("plugin.py"), code).await.unwrap();
    let asr = plugins.join("mock_asr");
    tokio::fs::create_dir_all(&asr).await.unwrap();
    for file in ["plugin.json", "plugin.py"] {
        tokio::fs::copy(
            sdk.join("plugins/builtin/mock_asr").join(file),
            asr.join(file),
        )
        .await
        .unwrap();
    }
    let python = test_python();
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
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: python.clone(),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: python.clone(),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let mut selections = Vec::new();
    let mut models = Vec::new();
    for service in services {
        let id = match service {
            AiService::Tts => "mock_tts",
            AiService::Asr => "mock_asr",
            _ => panic!("test only supports speech"),
        };
        let overlay = LockedOverlay {
            plugin_id: id.into(),
            revision: "fixture".into(),
            python_version: version.clone(),
            packages: vec![],
            actions: vec![],
        };
        let directory = overlays.installed_path(&overlay).unwrap();
        tokio::fs::create_dir_all(&directory).await.unwrap();
        tokio::fs::write(
            directory.join(".neuromita-overlay.json"),
            serde_json::to_vec(&overlay).unwrap(),
        )
        .await
        .unwrap();
        selections.push(ServiceSelection {
            service: *service,
            backends: vec![],
            overlays: vec![overlay],
        });
        models.push(ModelActivation {
            service: *service,
            plugin_id: id.into(),
            settings: json!({}),
            asset_revision: None,
        });
    }
    let composition =
        RuntimeComposition::resolve(topology, services[0], &selections, &backends, &overlays)
            .await
            .unwrap();
    let supervisor = AiSupervisor::new(
        topology,
        PythonHostConfig {
            python_executable: python,
            sdk_root: sdk,
            plugins_directory: plugins,
            models_directory: None,
            request_timeout: Duration::from_secs(5),
        },
    );
    (supervisor, composition, models)
}

async fn crash_tts(supervisor: &AiSupervisor) {
    assert!(
        supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({"kind":"tts", "input":{"crash": true}})
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn shared_recovery_reinitializes_both_models_and_rejects_stale_attempts() {
    let root = std::env::temp_dir().join(format!("nm-shared-recovery-{}", Uuid::new_v4()));
    let (supervisor, composition, models) = fixture(
        &root,
        ProcessTopology::Shared,
        &[AiService::Tts, AiService::Asr],
    )
    .await;
    let original = supervisor
        .activate_composed(AiService::Tts, &composition, &models)
        .await
        .unwrap();
    for service in [AiService::Tts, AiService::Asr] {
        let state = supervisor.status_snapshot().service(service);
        assert_eq!(state.generation, Some(original));
        assert_eq!(state.phase, RuntimePhase::ModelReady);
    }
    assert!(matches!(
        supervisor
            .recover_failed_composed(AiService::Tts, original, &composition, &models)
            .await,
        Err(AiRuntimeError::ActivationSuperseded)
    ));
    crash_tts(&supervisor).await;
    for service in [AiService::Tts, AiService::Asr] {
        let state = supervisor.status_snapshot().service(service);
        assert_eq!(state.generation, Some(original));
        assert_eq!(state.phase, RuntimePhase::Failed);
        assert!(state.active_model.is_none());
    }
    let mut invalid = models.clone();
    invalid[0].settings = json!({"speaker": "unknown"});
    assert!(
        supervisor
            .recover_failed_composed(AiService::Tts, original, &composition, &invalid)
            .await
            .is_err()
    );
    for service in [AiService::Tts, AiService::Asr] {
        let state = supervisor.status_snapshot().service(service);
        assert_eq!(state.generation, Some(original));
        assert_eq!(state.phase, RuntimePhase::Failed);
        assert!(state.active_model.is_none());
    }
    let replacement = supervisor
        .recover_failed_composed(AiService::Tts, original, &composition, &models)
        .await
        .unwrap();
    assert_ne!(original, replacement);
    for service in [AiService::Tts, AiService::Asr] {
        let state = supervisor.status_snapshot().service(service);
        assert_eq!(state.generation, Some(replacement));
        assert_eq!(state.phase, RuntimePhase::ModelReady);
        assert!(state.active_model.is_some());
    }
    let transcript = supervisor
        .call(
            AiService::Asr,
            "infer",
            json!({"kind": "asr", "input": {"mock_transcript": "restored"}}),
        )
        .await
        .unwrap();
    assert_eq!(transcript["text"], "restored");
    assert!(matches!(
        supervisor
            .recover_failed_composed(AiService::Tts, original, &composition, &models)
            .await,
        Err(AiRuntimeError::ActivationSuperseded)
    ));
    supervisor.stop(AiService::Tts).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn split_recovery_keeps_independent_asr_worker_untouched() {
    let root = std::env::temp_dir().join(format!("nm-split-recovery-{}", Uuid::new_v4()));
    let (supervisor, tts_composition, tts_models) =
        fixture(&root, ProcessTopology::Split, &[AiService::Tts]).await;
    let (_, asr_composition, asr_models) =
        fixture(&root, ProcessTopology::Split, &[AiService::Asr]).await;
    let tts_generation = supervisor
        .activate_composed(AiService::Tts, &tts_composition, &tts_models)
        .await
        .unwrap();
    let asr_generation = supervisor
        .activate_composed(AiService::Asr, &asr_composition, &asr_models)
        .await
        .unwrap();
    crash_tts(&supervisor).await;
    assert_eq!(
        supervisor.status_snapshot().service(AiService::Tts).phase,
        RuntimePhase::Failed
    );
    assert_eq!(
        supervisor.status_snapshot().service(AiService::Asr).phase,
        RuntimePhase::ModelReady
    );
    assert!(matches!(
        supervisor
            .recover_failed_composed(
                AiService::Asr,
                tts_generation,
                &asr_composition,
                &asr_models
            )
            .await,
        Err(AiRuntimeError::ActivationSuperseded)
    ));
    let recovered = supervisor
        .recover_failed_composed(
            AiService::Tts,
            tts_generation,
            &tts_composition,
            &tts_models,
        )
        .await
        .unwrap();
    assert_ne!(recovered, tts_generation);
    assert_eq!(
        supervisor.status_snapshot().service(AiService::Tts).phase,
        RuntimePhase::ModelReady
    );
    let asr_state = supervisor.status_snapshot().service(AiService::Asr);
    assert_eq!(asr_state.generation, Some(asr_generation));
    assert_eq!(asr_state.phase, RuntimePhase::ModelReady);
    supervisor.stop(AiService::Tts).await.unwrap();
    supervisor.stop(AiService::Asr).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn application_recovery_revalidates_generation_host_and_selection_guard() {
    let root = std::env::temp_dir().join(format!("nm-application-recovery-{}", Uuid::new_v4()));
    let (_, composition, models) = fixture(&root, ProcessTopology::Split, &[AiService::Tts]).await;
    let (_, asr_composition, asr_models) =
        fixture(&root, ProcessTopology::Split, &[AiService::Asr]).await;
    let build_plan = || AiApplicationConfig {
        topology: ProcessTopology::Split,
        host: PythonHostConfig {
            python_executable: test_python(),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: None,
            request_timeout: Duration::from_secs(5),
        },
        workers: vec![
            AiWorkerSelection {
                service: AiService::Tts,
                composition: composition.clone(),
                models: models.clone(),
            },
            AiWorkerSelection {
                service: AiService::Asr,
                composition: asr_composition.clone(),
                models: asr_models.clone(),
            },
        ],
        voice_profiles: None,
    };
    let running = build_plan().activate().await.unwrap();
    let asr_generation = running
        .status_snapshot()
        .service(AiService::Asr)
        .generation
        .unwrap();
    let original = running
        .status_snapshot()
        .service(AiService::Tts)
        .generation
        .unwrap();
    assert!(
        running
            .voice_provider()
            .synthesize(
                Arc::from("CRASH"),
                VoiceOptions {
                    voice: "default".into(),
                    volume: Volume::new(50).unwrap(),
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        running.refresh_status().await.service(AiService::Tts).phase,
        RuntimePhase::Failed
    );
    let mut mismatched = build_plan();
    mismatched.host.request_timeout = Duration::from_secs(10);
    assert!(
        running
            .recover_failed_worker(AiService::Tts, original, &mismatched, &|| Ok(()))
            .await
            .is_err()
    );
    let rejected = running
        .recover_failed_worker(AiService::Tts, original, &build_plan(), &|| {
            Err(AiRuntimeError::ActivationSuperseded)
        })
        .await;
    assert!(matches!(
        rejected,
        Err(
            neuromita::ai_runtime::application::AiApplicationError::Runtime(
                AiRuntimeError::ActivationSuperseded
            )
        )
    ));
    assert_eq!(
        running.status_snapshot().service(AiService::Tts).phase,
        RuntimePhase::Failed
    );
    let replacement = running
        .recover_failed_worker(AiService::Tts, original, &build_plan(), &|| Ok(()))
        .await
        .unwrap();
    assert_ne!(original, replacement);
    assert_eq!(
        running.status_snapshot().service(AiService::Tts).phase,
        RuntimePhase::ModelReady
    );
    let healthy_asr = running.status_snapshot().service(AiService::Asr);
    assert_eq!(healthy_asr.generation, Some(asr_generation));
    assert_eq!(healthy_asr.phase, RuntimePhase::ModelReady);
    assert!(matches!(
        running
            .recover_failed_worker(AiService::Tts, original, &build_plan(), &|| Ok(()))
            .await,
        Err(
            neuromita::ai_runtime::application::AiApplicationError::Runtime(
                AiRuntimeError::ActivationSuperseded
            )
        )
    ));
    running.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancel_inflight_python_initialize_keeps_failed_generation_retryable() {
    let root = std::env::temp_dir().join(format!("nm-recovery-cancel-{}", Uuid::new_v4()));
    let (supervisor, composition, models) =
        fixture(&root, ProcessTopology::Split, &[AiService::Tts]).await;
    let supervisor = Arc::new(supervisor);
    let original = supervisor
        .activate_composed(AiService::Tts, &composition, &models)
        .await
        .unwrap();
    crash_tts(&supervisor).await;
    let failed = supervisor.status_snapshot().service(AiService::Tts);
    assert_eq!(failed.phase, RuntimePhase::Failed);
    let crash_reason = failed.last_error.clone();

    let plugin_path = root.join("plugins/mock_tts/plugin.py");
    let blocked = root.join("block-initialize");
    let entered = root.join("initialize-entered-pid");
    let code = tokio::fs::read_to_string(&plugin_path).await.unwrap();
    let needle = "    def initialize(self, settings):\n";
    assert!(code.contains(needle));
    let hook = format!(
        "    def initialize(self, settings):\n        import os, time\n        from pathlib import Path\n        if Path({:?}).exists():\n            Path({:?}).write_text(str(os.getpid()), encoding='utf-8')\n            while True:\n                time.sleep(0.02)\n",
        blocked.to_string_lossy().to_string(),
        entered.to_string_lossy().to_string(),
    );
    tokio::fs::write(&plugin_path, code.replacen(needle, &hook, 1))
        .await
        .unwrap();
    tokio::fs::write(&blocked, "block").await.unwrap();

    let recovering = Arc::clone(&supervisor);
    let retry_composition = composition.clone();
    let retry_models = models.clone();
    let pending = tokio::spawn(async move {
        recovering
            .recover_failed_composed(AiService::Tts, original, &composition, &models)
            .await
    });
    let pid: u32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(&entered).await {
                return value.parse().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real Python initialize must enter the blocking fixture");
    let in_flight = supervisor.status_snapshot().service(AiService::Tts);
    assert_eq!(in_flight.phase, RuntimePhase::Failed);
    assert_eq!(in_flight.generation, Some(original));
    assert_eq!(in_flight.last_error, crash_reason);
    assert!(in_flight.active_model.is_none());

    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let path = format!("/proc/{pid}/status");
            let running = tokio::fs::read_to_string(path).await.is_ok_and(|state| {
                !state
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains("Z (zombie)"))
            });
            if !running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping the recovery future must terminate the Python candidate");
    let after_cancel = supervisor.refresh_status().await.service(AiService::Tts);
    assert_eq!(after_cancel.phase, RuntimePhase::Failed);
    assert_eq!(after_cancel.generation, Some(original));
    assert_eq!(after_cancel.last_error, crash_reason);

    tokio::fs::remove_file(&blocked).await.unwrap();
    let replacement = supervisor
        .recover_failed_composed(AiService::Tts, original, &retry_composition, &retry_models)
        .await
        .unwrap();
    assert_ne!(replacement, original);
    assert_eq!(
        supervisor.status_snapshot().service(AiService::Tts).phase,
        RuntimePhase::ModelReady
    );
    supervisor.stop(AiService::Tts).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn split_tts_model_switch_is_atomic_and_does_not_restart_healthy_asr() {
    let root = std::env::temp_dir().join(format!("nm-split-switch-{}", Uuid::new_v4()));
    let (supervisor, tts_composition, tts_models) =
        fixture(&root, ProcessTopology::Split, &[AiService::Tts]).await;
    let (_, asr_composition, asr_models) =
        fixture(&root, ProcessTopology::Split, &[AiService::Asr]).await;
    let original_tts = supervisor
        .activate_composed(AiService::Tts, &tts_composition, &tts_models)
        .await
        .unwrap();
    let original_asr = supervisor
        .activate_composed(AiService::Asr, &asr_composition, &asr_models)
        .await
        .unwrap();

    let alternate_id = "mock_tts_alt";
    let plugin = root.join("plugins").join(alternate_id);
    tokio::fs::create_dir_all(&plugin).await.unwrap();
    let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(sdk.join("plugins/builtin/mock_tts/plugin.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    manifest["id"] = json!(alternate_id);
    tokio::fs::write(
        plugin.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .await
    .unwrap();
    tokio::fs::copy(
        sdk.join("plugins/builtin/mock_tts/plugin.py"),
        plugin.join("plugin.py"),
    )
    .await
    .unwrap();
    let overlays = OverlayInstaller {
        uv_executable: root.join("uv"),
        python_executable: test_python(),
        overlays_directory: root.join("overlays"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let version = tts_composition.python_version().to_owned();
    let alternate_layer = LockedOverlay {
        plugin_id: alternate_id.into(),
        revision: "fixture".into(),
        python_version: version,
        packages: vec![],
        actions: vec![],
    };
    let alternate_path = overlays.installed_path(&alternate_layer).unwrap();
    tokio::fs::create_dir_all(&alternate_path).await.unwrap();
    tokio::fs::write(
        alternate_path.join(".neuromita-overlay.json"),
        serde_json::to_vec(&alternate_layer).unwrap(),
    )
    .await
    .unwrap();
    let backends = BackendInstaller {
        uv_executable: root.join("uv"),
        python_executable: test_python(),
        backends_directory: root.join("backends"),
        cache_directory: root.join("cache"),
        timeout: Duration::from_secs(5),
    };
    let selected = RuntimeComposition::resolve(
        ProcessTopology::Split,
        AiService::Tts,
        &[ServiceSelection {
            service: AiService::Tts,
            backends: vec![],
            overlays: vec![alternate_layer],
        }],
        &backends,
        &overlays,
    )
    .await
    .unwrap();
    let mut model = ModelActivation {
        service: AiService::Tts,
        plugin_id: alternate_id.into(),
        settings: json!({"speaker":"unknown"}),
        asset_revision: None,
    };
    assert!(
        supervisor
            .activate_composed(AiService::Tts, &selected, &[model.clone()])
            .await
            .is_err()
    );
    let before = supervisor.status_snapshot();
    assert_eq!(
        before.service(AiService::Tts).generation,
        Some(original_tts)
    );
    assert_eq!(
        before.service(AiService::Tts).phase,
        RuntimePhase::ModelReady
    );
    assert_eq!(
        before.service(AiService::Asr).generation,
        Some(original_asr)
    );
    assert_eq!(
        before.service(AiService::Asr).phase,
        RuntimePhase::ModelReady
    );

    model.settings = json!({"speaker":"bright"});
    let switched = supervisor
        .activate_composed(AiService::Tts, &selected, &[model])
        .await
        .unwrap();
    assert_ne!(switched, original_tts);
    assert_eq!(
        supervisor.selected_model(AiService::Tts).await.unwrap().0,
        alternate_id
    );
    let after = supervisor.status_snapshot();
    assert_eq!(after.service(AiService::Tts).generation, Some(switched));
    assert_eq!(
        after.service(AiService::Tts).phase,
        RuntimePhase::ModelReady
    );
    assert_eq!(after.service(AiService::Asr).generation, Some(original_asr));
    assert_eq!(
        after.service(AiService::Asr).phase,
        RuntimePhase::ModelReady
    );
    let transcript = supervisor
        .call(
            AiService::Asr,
            "infer",
            json!({"kind":"asr", "input": {"mock_transcript":"asr-unchanged"}}),
        )
        .await
        .unwrap();
    assert_eq!(transcript["text"], "asr-unchanged");
    supervisor.stop(AiService::Tts).await.unwrap();
    supervisor.stop(AiService::Asr).await.unwrap();
    tokio::fs::remove_dir_all(root).await.unwrap();
}
