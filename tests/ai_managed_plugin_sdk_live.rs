use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use neuromita::ai_runtime::{
    AiService, AiSupervisor, ProcessTopology, PythonHostConfig, fetch_install_plan,
    fetch_speech_plugin_inspection,
    managed::ManagedRuntimeBootstrap,
    uv::{UvArchiveKind, UvDistribution, UvInstaller},
};
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};
use tokio::process::Command;
use uuid::Uuid;

async fn real_uv_archive(source: &Path, root: &Path) -> (PathBuf, UvArchiveKind) {
    let directory = root.join("uv-distribution");
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    tokio::fs::copy(source, directory.join(name)).await.unwrap();
    if cfg!(windows) {
        let archive = root.join("uv.zip");
        let source = directory
            .join(name)
            .display()
            .to_string()
            .replace('"', "`\"");
        let destination = archive.display().to_string().replace('"', "`\"");
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Compress-Archive -LiteralPath \"{source}\" -DestinationPath \"{destination}\""
                ),
            ])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (archive, UvArchiveKind::Zip)
    } else {
        let archive = root.join("uv.tar.gz");
        let output = Command::new("tar")
            .args(["-czf"])
            .arg(&archive)
            .arg("-C")
            .arg(directory)
            .arg(name)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (archive, UvArchiveKind::TarGz)
    }
}

async fn write_plugin(root: &Path, source: &str, id: &str, manifest: Value, code: &str) {
    let directory = root.join("AIData/plugins").join(source).join(id);
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(directory.join("plugin.json"), manifest.to_string())
        .await
        .unwrap();
    tokio::fs::write(directory.join("plugin.py"), code)
        .await
        .unwrap();
}

const TTS_PLUGIN: &str = r#"from neuromita_ai_sdk import InstallPlan, ModelDescriptor, InstallVerification
class Plugin:
    def __init__(self):
        self.ready = False
        self.speaker = None
    def describe(self): return {"kind": "tts", "name": "Community voice"}
    def list_models(self): return (ModelDescriptor("community_voice", "Community Voice"),)
    def install_plan(self): return InstallPlan(supported_backends=("remote-api",))
    def probe_install(self): return {"installed": True, "missing": []}
    def verify_install(self): return InstallVerification(installed=True, verified=True)
    def settings_schema(self): return {"speaker": {"type": "string", "choices": ["alto", "bass"], "default": "alto"}}
    def initialize(self, settings):
        if settings.get("speaker", "alto") not in ("alto", "bass"):
            raise ValueError("unknown speaker")
        self.speaker = settings.get("speaker", "alto")
        self.ready = True
        return {"model_ready": True, "speaker": self.speaker}
    def infer(self, data):
        if not self.ready: raise ValueError("not ready")
        return {"text": data.get("text", ""), "speaker": self.speaker}
    def unload(self): self.ready = False
"#;

#[tokio::test]
#[ignore = "requires NEUROMITA_REAL_UV, downloads actual managed CPython 3.12.10"]
async fn actual_managed_cpython_runs_community_tts_and_builtin_asr_through_typed_sdk() {
    let executable =
        PathBuf::from(std::env::var_os("NEUROMITA_REAL_UV").expect("real uv required"));
    assert!(executable.is_absolute() && executable.is_file());
    let version = Command::new(&executable)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap();
    let version = version.split_whitespace().nth(1).unwrap().to_owned();
    assert_eq!(version, "0.10.0");

    let root = std::env::temp_dir().join(format!("neuromita-c1-live-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&root).await.unwrap();
    let (archive, archive_kind) = real_uv_archive(&executable, &root).await;
    let sha = digest(&SHA256, &tokio::fs::read(&archive).await.unwrap())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let bootstrap = ManagedRuntimeBootstrap {
        uv: UvInstaller {
            data_directory: root.join("AIData"),
            distribution: UvDistribution {
                version,
                archive_url: format!("file://{}", archive.display()),
                archive_sha256: sha,
                archive_kind,
            },
            timeout: Duration::from_secs(180),
        },
        python_version: "3.12.10".into(),
        python_timeout: Duration::from_secs(240),
    };
    let runtime = bootstrap.prepare().await.unwrap();
    assert!(
        runtime
            .python_executable
            .starts_with(root.join("AIData/bootstrap/python"))
    );
    let plugins = root.join("AIData/plugins");
    let builtin = Path::new(env!("CARGO_MANIFEST_DIR")).join("python/plugins/builtin/mock_asr");
    let builtin_root = plugins.join("builtin/mock_asr");
    tokio::fs::create_dir_all(&builtin_root).await.unwrap();
    for name in ["plugin.json", "plugin.py"] {
        tokio::fs::copy(builtin.join(name), builtin_root.join(name))
            .await
            .unwrap();
    }
    write_plugin(
        &root,
        "community",
        "community_voice",
        json!({"id":"community_voice","name":"Community Voice","version":"1.2.3","publisher":"tests","kind":"tts","api_version":1,"entrypoint":"plugin.py","permissions":[]}),
        TTS_PLUGIN,
    ).await;
    write_plugin(
        &root,
        "community",
        "bad_community",
        json!({"id":"bad_community","name":"Bad Plugin","version":"1.0.0","publisher":"tests","kind":"tts","api_version":2,"entrypoint":"plugin.py","permissions":[]}),
        "raise RuntimeError('bad plugin must not be imported')",
    ).await;
    write_plugin(
        &root,
        "community",
        "needs_approval",
        json!({"id":"needs_approval","name":"Approval","version":"1.0.0","publisher":"tests","kind":"tts","api_version":1,"entrypoint":"plugin.py","permissions":["network"]}),
        "raise RuntimeError('unapproved plugin must not be imported')",
    ).await;

    let config = PythonHostConfig {
        python_executable: runtime.python_executable.clone(),
        sdk_root: Path::new(env!("CARGO_MANIFEST_DIR")).join("python"),
        plugins_directory: plugins.clone(),
        models_directory: None,
        request_timeout: Duration::from_secs(20),
    };
    let plan = fetch_install_plan(&config, "community_voice")
        .await
        .unwrap();
    assert_eq!(plan.supported_backends, ["remote-api"]);
    assert!(fetch_install_plan(&config, "bad_community").await.is_err());
    assert!(fetch_install_plan(&config, "needs_approval").await.is_err());
    let inspection = fetch_speech_plugin_inspection(
        &config,
        AiService::Tts,
        "community_voice",
        json!({}),
        None,
        None,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(inspection.effective_settings["speaker"], "alto");
    assert!(inspection.install_probe.installed);
    assert_eq!(inspection.install_plan.supported_backends, ["remote-api"]);

    let supervisor = AiSupervisor::new(ProcessTopology::Shared, config);
    let generation = supervisor.start(AiService::Tts).await.unwrap();
    let initial = supervisor
        .call(AiService::Tts, "status", json!({}))
        .await
        .unwrap();
    assert_eq!(initial["services"], json!({}));
    let discovered = supervisor.discover(AiService::Tts).await.unwrap();
    let entries = discovered.as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|item| item["id"] == "community_voice" && item["discovery_status"] == "available")
    );
    assert!(
        entries
            .iter()
            .any(|item| item["id"] == "mock_asr" && item["discovery_status"] == "available")
    );
    assert!(
        entries
            .iter()
            .any(|item| item["id"] == "bad_community" && item["discovery_status"] == "invalid")
    );
    assert!(
        entries.iter().any(|item| item["id"] == "needs_approval"
            && item["discovery_status"] == "approval_required")
    );
    assert!(
        supervisor
            .call(
                AiService::Tts,
                "initialize",
                json!({"plugin_id":"bad_community","settings":{}})
            )
            .await
            .is_err()
    );
    assert_eq!(
        supervisor
            .call(AiService::Tts, "status", json!({}))
            .await
            .unwrap()["services"],
        json!({})
    );
    assert!(
        supervisor
            .call(
                AiService::Tts,
                "validate_config",
                json!({"plugin_id":"community_voice","settings":{"speaker":"other"}})
            )
            .await
            .is_err()
    );

    let tts = supervisor
        .initialize(AiService::Tts, "community_voice", json!({"speaker":"bass"}))
        .await
        .unwrap();
    assert_eq!(tts["model_ready"], true);
    assert_eq!(
        supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({"kind":"tts","input":{"text":"hi"}})
            )
            .await
            .unwrap()["speaker"],
        "bass"
    );
    let asr = supervisor
        .initialize(AiService::Asr, "mock_asr", json!({"language":"en"}))
        .await
        .unwrap();
    assert_eq!(asr["model_ready"], true);
    let result = supervisor
        .call(
            AiService::Asr,
            "infer",
            json!({"kind":"asr","input":{"mock_transcript":"recognized"}}),
        )
        .await
        .unwrap();
    assert_eq!(result["text"], "recognized");
    let status = supervisor
        .call(AiService::Tts, "status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["services"]["tts"]["model_ready"], true);
    assert_eq!(status["services"]["asr"]["model_ready"], true);
    assert_eq!(
        supervisor
            .start(AiService::Tts)
            .await
            .unwrap_err()
            .to_string(),
        "worker already started for the requested service"
    );
    supervisor.stop(AiService::Tts).await.unwrap();
    assert!(
        supervisor
            .call(AiService::Tts, "status", json!({}))
            .await
            .is_err()
    );
    assert_ne!(generation, Uuid::nil());
    tokio::fs::remove_dir_all(root).await.unwrap();
}
