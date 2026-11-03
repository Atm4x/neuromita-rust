//! Rust-owned process lifecycle and correlated JSON-lines IPC for Python AI plugins.

pub mod assets;
pub mod backend;
pub mod bootstrap;
pub mod composition;
mod distribution;
pub mod install;
mod layer;
pub mod managed;
pub mod overlay;
pub mod resolver;
pub mod uv;
pub mod voice_profile;

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};
use uuid::Uuid;

use crate::voice::{
    AudioArtifact, AudioFormat, VoiceError, VoiceFuture, VoiceOptions, VoiceProvider,
};

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const IPC_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTopology {
    Shared,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiService {
    Tts,
    Asr,
    Embedding,
    Beats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WorkerKey {
    Shared,
    Service(AiService),
}

impl ProcessTopology {
    fn key(self, service: AiService) -> WorkerKey {
        match self {
            Self::Shared => WorkerKey::Shared,
            Self::Split => WorkerKey::Service(service),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PythonHostConfig {
    pub python_executable: PathBuf,
    pub sdk_root: PathBuf,
    pub plugins_directory: PathBuf,
    pub models_directory: Option<PathBuf>,
    pub request_timeout: Duration,
}

#[derive(Debug, Error)]
pub enum AiRuntimeError {
    #[error("worker is not running for the requested service")]
    NotRunning,
    #[error("worker already started for the requested service")]
    AlreadyRunning,
    #[error("worker generation changed before request completed")]
    StaleGeneration,
    #[error("python worker failed to start: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("python worker IPC failed: {0}")]
    Io(#[source] std::io::Error),
    #[error("python worker timed out")]
    Timeout,
    #[error("invalid Python host protocol: {0}")]
    Protocol(String),
    #[error("Python plugin rejected request ({kind}): {message}")]
    Plugin { kind: String, message: String },
}

#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    message_type: String,
    api_version: u32,
}

#[derive(Serialize)]
struct Request<'a> {
    id: &'a str,
    op: &'a str,
    args: &'a Value,
}

#[derive(Deserialize)]
struct Response {
    id: String,
    ok: bool,
    #[serde(default)]
    result: Value,
    error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct RemoteError {
    kind: String,
    message: String,
}

struct WorkerProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    timeout: Duration,
    generation: Uuid,
    failed: bool,
    selections: HashMap<AiService, (String, Option<String>)>,
}

impl WorkerProcess {
    async fn launch(
        config: &PythonHostConfig,
        layers: &[PathBuf],
        models: &[ModelActivation],
    ) -> Result<Self, AiRuntimeError> {
        let mut imports = vec![config.sdk_root.clone()];
        imports.extend(layers.iter().cloned());
        let python_path = std::env::join_paths(&imports)
            .map_err(|e| AiRuntimeError::Protocol(format!("invalid Python import path: {e}")))?;
        let mut asset_revisions = serde_json::Map::new();
        if let Some(root) = &config.models_directory {
            for model in models {
                if let Some(revision) = &model.asset_revision {
                    let store = assets::ModelAssetStore {
                        root: root.clone(),
                        max_asset_bytes: 4 * 1024 * 1024 * 1024,
                        timeout: config.request_timeout,
                    };
                    store
                        .verified_revision(&model.plugin_id, revision)
                        .await
                        .map_err(|error| {
                            AiRuntimeError::Protocol(format!("unverified model revision: {error}"))
                        })?;
                    asset_revisions
                        .insert(model.plugin_id.clone(), Value::String(revision.clone()));
                }
            }
        } else if models.iter().any(|model| model.asset_revision.is_some()) {
            return Err(AiRuntimeError::Protocol(
                "model revision requires a model asset root".into(),
            ));
        }
        let mut command = Command::new(&config.python_executable);
        command
            .env_remove("NEUROMITA_AI_MODELS_DIR")
            .env_remove("NEUROMITA_AI_MODEL_REVISIONS");
        if let Some(root) = &config.models_directory {
            if !root.is_absolute() {
                return Err(AiRuntimeError::Protocol(
                    "model root must be absolute".into(),
                ));
            }
            command.env("NEUROMITA_AI_MODELS_DIR", root);
            command.env(
                "NEUROMITA_AI_MODEL_REVISIONS",
                Value::Object(asset_revisions).to_string(),
            );
        }

        command
            .arg("-m")
            .arg("neuromita_ai_sdk.host")
            .arg("--plugins-dir")
            .arg(&config.plugins_directory)
            .env("PYTHONPATH", python_path)
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env_remove("PYTHONHOME")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(AiRuntimeError::Spawn)?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| AiRuntimeError::Protocol("stdin unavailable".into()))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| AiRuntimeError::Protocol("stdout unavailable".into()))?;
        let mut worker = Self {
            child,
            input,
            output: BufReader::new(output),
            timeout: config.request_timeout,
            generation: Uuid::new_v4(),
            failed: false,
            selections: HashMap::new(),
        };
        let line = worker.read_frame().await?;
        let hello: Hello =
            serde_json::from_str(&line).map_err(|e| AiRuntimeError::Protocol(e.to_string()))?;
        if hello.message_type != "hello" || hello.api_version != IPC_VERSION {
            return Err(AiRuntimeError::Protocol(
                "incompatible Python host handshake".into(),
            ));
        }
        Ok(worker)
    }

    async fn read_frame(&mut self) -> Result<String, AiRuntimeError> {
        let read = async {
            let mut frame = Vec::new();
            loop {
                let available = self.output.fill_buf().await.map_err(AiRuntimeError::Io)?;
                if available.is_empty() {
                    return Err(AiRuntimeError::Protocol("worker closed stdout".into()));
                }
                let newline = available.iter().position(|byte| *byte == b'\n');
                let length = newline.map_or(available.len(), |pos| pos + 1);
                if frame.len() + length > MAX_RESPONSE_BYTES {
                    return Err(AiRuntimeError::Protocol(
                        "worker frame exceeds size limit".into(),
                    ));
                }
                frame.extend_from_slice(&available[..length]);
                self.output.consume(length);
                if newline.is_some() {
                    break;
                }
            }
            String::from_utf8(frame).map_err(|err| AiRuntimeError::Protocol(err.to_string()))
        };
        timeout(self.timeout, read)
            .await
            .map_err(|_| AiRuntimeError::Timeout)?
    }

    async fn request(&mut self, operation: &str, args: &Value) -> Result<Value, AiRuntimeError> {
        if self.failed {
            return Err(AiRuntimeError::Protocol("worker requires restart".into()));
        }
        let id = Uuid::new_v4().to_string();
        let mut request = serde_json::to_vec(&Request {
            id: &id,
            op: operation,
            args,
        })
        .map_err(|e| AiRuntimeError::Protocol(e.to_string()))?;
        request.push(b'\n');
        if request.len() > MAX_REQUEST_BYTES {
            return Err(AiRuntimeError::Protocol(
                "request exceeds size limit".into(),
            ));
        }
        let deadline = self.timeout;
        let outcome = async {
            self.input
                .write_all(&request)
                .await
                .map_err(AiRuntimeError::Io)?;
            self.input.flush().await.map_err(AiRuntimeError::Io)?;
            let frame = self.read_frame().await?;
            let response: Response = serde_json::from_str(&frame)
                .map_err(|e| AiRuntimeError::Protocol(e.to_string()))?;
            if response.id != id {
                return Err(AiRuntimeError::Protocol("correlation id mismatch".into()));
            }
            if !response.ok {
                let error = response
                    .error
                    .ok_or_else(|| AiRuntimeError::Protocol("missing remote error".into()))?;
                return Err(AiRuntimeError::Plugin {
                    kind: error.kind,
                    message: error.message,
                });
            }
            Ok(response.result)
        };
        match timeout(deadline, outcome).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error @ AiRuntimeError::Plugin { .. })) => Err(error),
            Ok(Err(error)) => {
                self.failed = true;
                let _ = self.child.start_kill();
                Err(error)
            }
            Err(_) => {
                self.failed = true;
                let _ = self.child.start_kill();
                Err(AiRuntimeError::Timeout)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelActivation {
    pub service: AiService,
    pub plugin_id: String,
    pub settings: Value,
    pub asset_revision: Option<String>,
}

impl AiService {
    fn kind(self) -> &'static str {
        match self {
            Self::Tts => "tts",
            Self::Asr => "asr",
            Self::Embedding => "embedding",
            Self::Beats => "beats",
        }
    }
}

pub struct AiSupervisor {
    topology: ProcessTopology,
    config: PythonHostConfig,
    workers: Mutex<HashMap<WorkerKey, Arc<Mutex<WorkerProcess>>>>,
    activation_locks: Mutex<HashMap<WorkerKey, Arc<Mutex<()>>>>,
}

impl AiSupervisor {
    pub fn new(topology: ProcessTopology, config: PythonHostConfig) -> Self {
        Self {
            topology,
            config,
            workers: Mutex::new(HashMap::new()),
            activation_locks: Mutex::new(HashMap::new()),
        }
    }

    async fn activation_lock(&self, key: WorkerKey) -> Arc<Mutex<()>> {
        self.activation_locks
            .lock()
            .await
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn start(&self, service: AiService) -> Result<Uuid, AiRuntimeError> {
        let key = self.topology.key(service);
        let slot = self.activation_lock(key).await;
        let _guard = slot.lock().await;
        if self.workers.lock().await.contains_key(&key) {
            return Err(AiRuntimeError::AlreadyRunning);
        }
        let worker = WorkerProcess::launch(&self.config, &[], &[]).await?;
        let generation = worker.generation;
        self.workers
            .lock()
            .await
            .insert(key, Arc::new(Mutex::new(worker)));
        Ok(generation)
    }

    pub async fn start_composed(
        &self,
        service: AiService,
        composition: &composition::RuntimeComposition,
    ) -> Result<Uuid, AiRuntimeError> {
        if !composition.services().contains(&service) {
            return Err(AiRuntimeError::Protocol(
                "service absent from resolved composition".into(),
            ));
        }
        if self.topology == ProcessTopology::Split && composition.services() != [service] {
            return Err(AiRuntimeError::Protocol(
                "split worker contains another service".into(),
            ));
        }
        let key = self.topology.key(service);
        let slot = self.activation_lock(key).await;
        let _guard = slot.lock().await;
        if self.workers.lock().await.contains_key(&key) {
            return Err(AiRuntimeError::AlreadyRunning);
        }
        let worker = WorkerProcess::launch(&self.config, composition.paths(), &[]).await?;
        let generation = worker.generation;
        self.workers
            .lock()
            .await
            .insert(key, Arc::new(Mutex::new(worker)));
        Ok(generation)
    }

    /// Prepare a new worker and initialize every selected service before publication.
    /// Failed candidates never replace a working process.
    pub async fn activate_composed(
        &self,
        service: AiService,
        composition: &composition::RuntimeComposition,
        models: &[ModelActivation],
    ) -> Result<Uuid, AiRuntimeError> {
        if !composition.services().contains(&service)
            || (self.topology == ProcessTopology::Split && composition.services() != [service])
        {
            return Err(AiRuntimeError::Protocol(
                "invalid service composition".into(),
            ));
        }
        self.reconfigure(service, composition.services(), composition.paths(), models)
            .await
    }

    async fn reconfigure(
        &self,
        service: AiService,
        services: &[AiService],
        layers: &[PathBuf],
        models: &[ModelActivation],
    ) -> Result<Uuid, AiRuntimeError> {
        use std::collections::HashSet;
        let selected: HashSet<_> = services.iter().copied().collect();
        let configured: HashSet<_> = models.iter().map(|model| model.service).collect();
        if selected.len() != services.len()
            || selected != configured
            || models.len() != configured.len()
            || !selected.contains(&service)
            || (self.topology == ProcessTopology::Split && selected != HashSet::from([service]))
        {
            return Err(AiRuntimeError::Protocol(
                "provide exactly one model per selected service".into(),
            ));
        }
        let key = self.topology.key(service);
        let slot = self.activation_lock(key).await;
        let _guard = slot.lock().await;
        let mut candidate = WorkerProcess::launch(&self.config, layers, models).await?;
        let prepare = async {
            let available = candidate.request("discover", &json!({})).await?;
            let plugins = available
                .as_array()
                .ok_or_else(|| AiRuntimeError::Protocol("invalid discovery response".into()))?;
            for model in models {
                if !plugins.iter().any(|plugin| {
                    plugin["id"] == model.plugin_id
                        && plugin["kind"] == model.service.kind()
                        && plugin["discovery_status"] == "available"
                }) {
                    return Err(AiRuntimeError::Protocol(format!(
                        "selected plugin {} does not support {}",
                        model.plugin_id,
                        model.service.kind()
                    )));
                }
                let ready = candidate
                    .request(
                        "initialize",
                        &json!({"plugin_id": model.plugin_id, "settings": model.settings}),
                    )
                    .await?;
                if ready["model_ready"] != true {
                    return Err(AiRuntimeError::Protocol(format!(
                        "model {} did not report ready",
                        model.plugin_id
                    )));
                }
                candidate.selections.insert(
                    model.service,
                    (model.plugin_id.clone(), model.asset_revision.clone()),
                );
            }
            Ok(())
        }
        .await;
        if let Err(error) = prepare {
            let _ = candidate.child.start_kill();
            let _ = candidate.child.wait().await;
            return Err(error);
        }
        let generation = candidate.generation;
        let old = self
            .workers
            .lock()
            .await
            .insert(key, Arc::new(Mutex::new(candidate)));
        if let Some(old) = old {
            let mut old = old.lock().await;
            let _ = old.child.start_kill();
            let _ = old.child.wait().await;
        }
        Ok(generation)
    }

    pub async fn stop(&self, service: AiService) -> Result<(), AiRuntimeError> {
        let key = self.topology.key(service);
        let slot = self.activation_lock(key).await;
        let _guard = slot.lock().await;
        let worker = self.workers.lock().await.remove(&key);
        let Some(worker) = worker else {
            return Err(AiRuntimeError::NotRunning);
        };
        let mut worker = worker.lock().await;
        let _ = worker.child.start_kill();
        let _ = worker.child.wait().await;
        Ok(())
    }

    pub async fn call(
        &self,
        service: AiService,
        operation: &str,
        args: Value,
    ) -> Result<Value, AiRuntimeError> {
        let worker = self
            .workers
            .lock()
            .await
            .get(&self.topology.key(service))
            .cloned()
            .ok_or(AiRuntimeError::NotRunning)?;
        let guard = worker.lock().await;
        let generation = guard.generation;
        drop(guard);
        let result = worker.lock().await.request(operation, &args).await;
        let current = self
            .workers
            .lock()
            .await
            .get(&self.topology.key(service))
            .cloned();
        let current_generation = if let Some(current) = current {
            Some(current.lock().await.generation)
        } else {
            None
        };
        if current_generation != Some(generation) {
            return Err(AiRuntimeError::StaleGeneration);
        }
        result
    }

    pub async fn discover(&self, service: AiService) -> Result<Value, AiRuntimeError> {
        self.call(service, "discover", json!({})).await
    }

    pub async fn initialize(
        &self,
        service: AiService,
        plugin_id: &str,
        settings: Value,
    ) -> Result<Value, AiRuntimeError> {
        let result = self
            .call(
                service,
                "initialize",
                json!({"plugin_id": plugin_id, "settings": settings}),
            )
            .await?;
        if result["model_ready"] == true {
            let worker = self
                .workers
                .lock()
                .await
                .get(&self.topology.key(service))
                .cloned();
            if let Some(worker) = worker {
                let mut guard = worker.lock().await;
                let revision = guard
                    .selections
                    .get(&service)
                    .filter(|(selected, _)| selected == plugin_id)
                    .and_then(|(_, revision)| revision.clone());
                guard
                    .selections
                    .insert(service, (plugin_id.into(), revision));
            }
        }
        Ok(result)
    }

    pub async fn selected_model(&self, service: AiService) -> Option<(String, Option<String>)> {
        let worker = self
            .workers
            .lock()
            .await
            .get(&self.topology.key(service))
            .cloned()?;
        worker.lock().await.selections.get(&service).cloned()
    }
}

/// Inspect an uninstalled plugin using an isolated, short-lived Python process.
/// Installation contracts do not run inside a live inference worker.
pub async fn fetch_install_plan(
    config: &PythonHostConfig,
    plugin_id: &str,
) -> Result<install::PluginInstallPlan, AiRuntimeError> {
    if !layer::valid_segment(plugin_id) {
        return Err(AiRuntimeError::Protocol("invalid plugin identifier".into()));
    }
    let mut worker = WorkerProcess::launch(config, &[], &[]).await?;
    let result = worker
        .request("install_plan", &json!({ "plugin_id": plugin_id }))
        .await;
    let _ = worker.child.start_kill();
    let _ = worker.child.wait().await;
    let value = result?;
    serde_json::from_value(value)
        .map_err(|error| AiRuntimeError::Protocol(format!("invalid install plan: {error}")))
}

pub struct PythonVoiceProvider {
    supervisor: Arc<AiSupervisor>,
    profiles: Option<Arc<voice_profile::VoiceProfileStore>>,
    service: AiService,
}

impl PythonVoiceProvider {
    pub fn new(supervisor: Arc<AiSupervisor>) -> Self {
        Self {
            supervisor,
            profiles: None,
            service: AiService::Tts,
        }
    }

    pub fn with_profiles(mut self, profiles: Arc<voice_profile::VoiceProfileStore>) -> Self {
        self.profiles = Some(profiles);
        self
    }
}

impl VoiceProvider for PythonVoiceProvider {
    fn synthesize(&self, text: Arc<str>, options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async move {
            let mut input =
                json!({"text": text, "volume": f64::from(options.volume.get()) / 100.0});
            if options.voice != "default" && !options.voice.is_empty() {
                let profiles = self.profiles.as_ref().ok_or_else(|| {
                    VoiceError::Generation("voice profile repository is not configured".into())
                })?;
                let profile = profiles
                    .load(&options.voice)
                    .await
                    .map_err(|error| VoiceError::Generation(error.to_string()))?;
                let selection = self
                    .supervisor
                    .selected_model(self.service)
                    .await
                    .ok_or_else(|| VoiceError::Generation("no selected TTS model".into()))?;
                if selection.0 != profile.plugin_id
                    || selection.1.as_deref() != Some(&profile.model_revision)
                {
                    return Err(VoiceError::Generation(
                        "voice profile does not match active TTS model revision".into(),
                    ));
                }
                input["settings"] = Value::Object(profile.settings);
                if let Some(path) = profile.reference_audio {
                    input["reference_audio"] = Value::String(path.to_string_lossy().into_owned());
                }
                if let Some(reference) = profile.reference_text {
                    input["reference_text"] = Value::String(reference);
                }
            }
            let result = self
                .supervisor
                .call(
                    self.service,
                    "infer",
                    json!({
                        "kind": "tts", "input": input
                    }),
                )
                .await
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            if result.get("format").and_then(Value::as_str) != Some("wav") {
                return Err(VoiceError::Generation("unsupported audio format".into()));
            }
            let encoded = result
                .get("audio_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| VoiceError::Generation("missing audio payload".into()))?;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| VoiceError::Generation(error.to_string()))?;
            if decoded.len() < 44 || &decoded[..4] != b"RIFF" || &decoded[8..12] != b"WAVE" {
                return Err(VoiceError::Generation(
                    "Python TTS returned invalid WAV".into(),
                ));
            }
            Ok(AudioArtifact {
                format: AudioFormat::Wave,
                bytes: decoded.into(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_python() -> String {
        std::env::var("NEUROMITA_TEST_PYTHON")
            .unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.to_owned())
    }

    #[test]
    fn slots_follow_topology() {
        assert_eq!(
            ProcessTopology::Shared.key(AiService::Tts),
            ProcessTopology::Shared.key(AiService::Asr)
        );
        assert_ne!(
            ProcessTopology::Split.key(AiService::Tts),
            ProcessTopology::Split.key(AiService::Asr)
        );
    }

    #[tokio::test]
    async fn model_revision_is_verified_and_scoped_to_selected_plugin() {
        use super::assets::{AssetInput, AssetSource, ModelAssetStore};
        use super::install::{ModelAssetRequirement, PluginInstallPlan};
        let root = std::env::temp_dir().join(format!("nm-model-inference-{}", Uuid::new_v4()));
        let plugins = root.join("plugins/fixture");
        tokio::fs::create_dir_all(&plugins).await.unwrap();
        tokio::fs::write(plugins.join("plugin.json"), r#"{"id":"fixture","name":"Fixture","kind":"tts","entrypoint":"plugin.py","api_version":1}"#).await.unwrap();
        tokio::fs::write(
            plugins.join("plugin.py"),
            r#"import os
from pathlib import Path
from neuromita_ai_sdk import InstallPlan
class Plugin:
    def describe(self): return {'kind': 'tts'}
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {'installed': True}
    def settings_schema(self): return {}
    def initialize(self, settings):
        import json
        rev = json.loads(os.environ['NEUROMITA_AI_MODEL_REVISIONS'])['fixture']
        root = Path(os.environ['NEUROMITA_AI_MODELS_DIR']) / 'fixture' / rev
        if (root / 'weights.bin').read_bytes() != b'weights': raise ValueError('unverified weights')
        return {'model_ready': True, 'revision': rev}
    def infer(self, data): return {'text': 'ok'}
    def unload(self): pass
"#,
        )
        .await
        .unwrap();
        let source = root.join("payload.bin");
        tokio::fs::write(&source, b"weights").await.unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA256, b"weights");
        let sha: String = digest.as_ref().iter().map(|x| format!("{x:02x}")).collect();
        let store = ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 1024,
            timeout: Duration::from_secs(3),
        };
        let plan = PluginInstallPlan {
            assets: vec![ModelAssetRequirement {
                role: "model".into(),
                relative_path: "weights.bin".into(),
                required: true,
                sha256: Some(sha.clone()),
            }],
            packages: vec![],
            requirements: vec![],
            backend: None,
            supported_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
        };
        store
            .publish(
                "fixture",
                "rev1",
                &plan,
                &[AssetInput {
                    role: "model".into(),
                    relative_path: "weights.bin".into(),
                    sha256: sha,
                    source: AssetSource::LocalFile(source),
                }],
            )
            .await
            .unwrap();
        let supervisor = AiSupervisor::new(
            ProcessTopology::Split,
            PythonHostConfig {
                python_executable: test_python().into(),
                sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
                plugins_directory: root.join("plugins"),
                models_directory: Some(root.join("models")),
                request_timeout: Duration::from_secs(5),
            },
        );
        let good = ModelActivation {
            service: AiService::Tts,
            plugin_id: "fixture".into(),
            settings: json!({}),
            asset_revision: Some("rev1".into()),
        };
        supervisor
            .reconfigure(
                AiService::Tts,
                &[AiService::Tts],
                &[],
                std::slice::from_ref(&good),
            )
            .await
            .unwrap();
        let result = supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({"kind":"tts","input":{"text":"ok"}}),
            )
            .await
            .unwrap();
        assert_eq!(result["text"], "ok");
        let bad = ModelActivation {
            asset_revision: Some("absent".into()),
            ..good
        };
        assert!(
            supervisor
                .reconfigure(AiService::Tts, &[AiService::Tts], &[], &[bad])
                .await
                .is_err()
        );
        assert!(
            supervisor
                .call(
                    AiService::Tts,
                    "infer",
                    json!({"kind":"tts","input":{"text":"still ready"}})
                )
                .await
                .is_ok()
        );
        supervisor.stop(AiService::Tts).await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn voice_profile_flows_to_python_inference_with_normalized_volume() {
        use super::assets::{AssetInput, AssetSource, ModelAssetStore};
        use super::install::{ModelAssetRequirement, PluginInstallPlan};
        use super::voice_profile::{VoiceProfile, VoiceProfileStore, VoiceReference};
        let root = std::env::temp_dir().join(format!("nm-profile-e2e-{}", Uuid::new_v4()));
        let plugin = root.join("plugins/test_voice");
        tokio::fs::create_dir_all(&plugin).await.unwrap();
        tokio::fs::write(plugin.join("plugin.json"), r#"{"id":"test_voice","name":"Test Voice","kind":"tts","entrypoint":"plugin.py","api_version":1}"#).await.unwrap();
        tokio::fs::write(
            plugin.join("plugin.py"),
            r#"from neuromita_ai_sdk import InstallPlan
import io, wave, base64
class Plugin:
    def describe(self): return {'kind': 'tts'}
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {'installed': True}
    def settings_schema(self): return {'silero.speaker_id': {'type':'string','default':'kseniya'}}
    def initialize(self, settings): return {'model_ready': True}
    def infer(self, data):
        assert data['settings']['silero.speaker_id'] == 'kseniya'
        assert data['reference_text'] == 'voice prompt'
        assert data['volume'] == 0.5
        assert open(data['reference_audio'], 'rb').read() == b'RIFF1234WAVE'
        buffer = io.BytesIO()
        with wave.open(buffer, 'wb') as output:
            output.setnchannels(1)
            output.setsampwidth(2)
            output.setframerate(16000)
            output.writeframes(b'\0\0' * 80)
        return {'format':'wav','audio_base64':base64.b64encode(buffer.getvalue()).decode('ascii')}
    def unload(self): pass
"#,
        )
        .await
        .unwrap();
        let weight = root.join("weight.bin");
        tokio::fs::write(&weight, b"weights").await.unwrap();
        let digest = |data: &[u8]| -> String {
            ring::digest::digest(&ring::digest::SHA256, data)
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        let model_store = ModelAssetStore {
            root: root.join("models"),
            max_asset_bytes: 1024,
            timeout: Duration::from_secs(3),
        };
        let plan = PluginInstallPlan {
            packages: vec![],
            requirements: vec![],
            backend: None,
            supported_backends: vec![],
            supported_python_minors: vec![],
            actions: vec![],
            assets: vec![ModelAssetRequirement {
                role: "weights".into(),
                relative_path: "model.bin".into(),
                required: true,
                sha256: Some(digest(b"weights")),
            }],
        };
        model_store
            .publish(
                "test_voice",
                "r1",
                &plan,
                &[AssetInput {
                    role: "weights".into(),
                    relative_path: "model.bin".into(),
                    sha256: digest(b"weights"),
                    source: AssetSource::LocalFile(weight),
                }],
            )
            .await
            .unwrap();
        let profiles = root.join("voices/crazy");
        tokio::fs::create_dir_all(profiles.join("references"))
            .await
            .unwrap();
        tokio::fs::write(profiles.join("references/voice.wav"), b"RIFF1234WAVE")
            .await
            .unwrap();
        tokio::fs::write(profiles.join("references/voice.txt"), b"voice prompt")
            .await
            .unwrap();
        let voice = VoiceProfile {
            id: "crazy".into(),
            plugin_id: "test_voice".into(),
            model_revision: "r1".into(),
            settings: serde_json::from_value(json!({"silero.speaker_id":"kseniya"})).unwrap(),
            reference_audio: Some(VoiceReference {
                relative_path: "references/voice.wav".into(),
                sha256: digest(b"RIFF1234WAVE"),
            }),
            reference_text: Some(VoiceReference {
                relative_path: "references/voice.txt".into(),
                sha256: digest(b"voice prompt"),
            }),
        };
        tokio::fs::write(
            profiles.join("voice.json"),
            serde_json::to_vec(&voice).unwrap(),
        )
        .await
        .unwrap();
        let supervisor = Arc::new(AiSupervisor::new(
            ProcessTopology::Split,
            PythonHostConfig {
                python_executable: test_python().into(),
                sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
                plugins_directory: root.join("plugins"),
                models_directory: Some(root.join("models")),
                request_timeout: Duration::from_secs(5),
            },
        ));
        supervisor
            .reconfigure(
                AiService::Tts,
                &[AiService::Tts],
                &[],
                &[ModelActivation {
                    service: AiService::Tts,
                    plugin_id: "test_voice".into(),
                    asset_revision: Some("r1".into()),
                    settings: json!({}),
                }],
            )
            .await
            .unwrap();
        let provider = PythonVoiceProvider::new(supervisor.clone()).with_profiles(Arc::new(
            VoiceProfileStore {
                root: root.join("voices"),
            },
        ));
        let audio = provider
            .synthesize(
                Arc::from("Привет"),
                VoiceOptions {
                    voice: "crazy".into(),
                    volume: crate::settings::Volume::new(50).unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(audio.format, AudioFormat::Wave);
        assert!(audio.bytes.starts_with(b"RIFF"));
        let mut invalid = voice;
        invalid.model_revision = "another".into();
        tokio::fs::write(
            profiles.join("voice.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            provider
                .synthesize(
                    Arc::from("ignored"),
                    VoiceOptions {
                        voice: "crazy".into(),
                        volume: crate::settings::Volume::new(50).unwrap(),
                    }
                )
                .await
                .is_err()
        );
        supervisor.stop(AiService::Tts).await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn split_activation_does_not_block_unrelated_asr_calls() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let temp = std::env::temp_dir().join(format!("nm-split-{}", Uuid::new_v4()));
        let plugins = temp.join("plugins");
        for id in ["mock_tts", "mock_asr"] {
            let src = source.join("plugins/builtin").join(id);
            let dest = plugins.join(id);
            tokio::fs::create_dir_all(&dest).await.unwrap();
            tokio::fs::copy(src.join("plugin.json"), dest.join("plugin.json"))
                .await
                .unwrap();
            let mut code = tokio::fs::read_to_string(src.join("plugin.py"))
                .await
                .unwrap()
                .replace("\r\n", "\n");
            if id == "mock_tts" {
                let needle = "    def initialize(self, settings):\n";
                assert!(code.contains(needle));
                code = code.replace(
                    needle,
                    "    def initialize(self, settings):\n        import time\n        time.sleep(0.6)\n",
                );
            }
            tokio::fs::write(dest.join("plugin.py"), code)
                .await
                .unwrap();
        }
        let python = test_python();
        let supervisor = Arc::new(AiSupervisor::new(
            ProcessTopology::Split,
            PythonHostConfig {
                python_executable: python.into(),
                sdk_root: source.clone(),
                plugins_directory: plugins,
                models_directory: None,
                request_timeout: Duration::from_secs(10),
            },
        ));
        supervisor.start(AiService::Asr).await.unwrap();
        supervisor
            .initialize(AiService::Asr, "mock_asr", json!({}))
            .await
            .unwrap();
        let next = Arc::clone(&supervisor);
        let activation = tokio::spawn(async move {
            next.reconfigure(
                AiService::Tts,
                &[AiService::Tts],
                &[],
                &[ModelActivation {
                    service: AiService::Tts,
                    plugin_id: "mock_tts".into(),
                    settings: json!({}),
                    asset_revision: None,
                }],
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        let response = timeout(
            Duration::from_millis(300),
            supervisor.call(
                AiService::Asr,
                "infer",
                json!({"kind":"asr", "input":{"mock_transcript":"unblocked"}}),
            ),
        )
        .await
        .expect("TTS initialization must not block ASR")
        .unwrap();
        assert_eq!(response["text"], "unblocked");
        activation.await.unwrap().unwrap();
        supervisor.stop(AiService::Tts).await.unwrap();
        supervisor.stop(AiService::Asr).await.unwrap();
        tokio::fs::remove_dir_all(temp).await.unwrap();
    }

    #[tokio::test]
    async fn candidate_activation_rolls_back_on_failure_and_replaces_on_success() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let python = test_python();
        let supervisor = AiSupervisor::new(
            ProcessTopology::Split,
            PythonHostConfig {
                python_executable: python.into(),
                sdk_root: root.clone(),
                plugins_directory: root.join("plugins/builtin"),
                models_directory: None,
                request_timeout: Duration::from_secs(10),
            },
        );
        let initial = supervisor.start(AiService::Tts).await.unwrap();
        supervisor
            .initialize(AiService::Tts, "mock_tts", json!({"speaker": "default"}))
            .await
            .unwrap();
        let before = supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({
                    "kind": "tts", "input": { "text": "Hello" }
                }),
            )
            .await
            .unwrap();
        let bad = ModelActivation {
            service: AiService::Tts,
            plugin_id: "mock_tts".into(),
            settings: json!({"speaker": "does-not-exist"}),
            asset_revision: None,
        };
        assert!(
            supervisor
                .reconfigure(AiService::Tts, &[AiService::Tts], &[], &[bad])
                .await
                .is_err()
        );
        let still = supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({
                    "kind": "tts", "input": { "text": "Hello" }
                }),
            )
            .await
            .unwrap();
        assert_eq!(before["audio_base64"], still["audio_base64"]);
        let good = ModelActivation {
            service: AiService::Tts,
            plugin_id: "mock_tts".into(),
            settings: json!({"speaker": "bright"}),
            asset_revision: None,
        };
        let next = supervisor
            .reconfigure(AiService::Tts, &[AiService::Tts], &[], &[good])
            .await
            .unwrap();
        assert_ne!(initial, next);
        let after = supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({
                    "kind": "tts", "input": { "text": "Hello" }
                }),
            )
            .await
            .unwrap();
        assert_ne!(before["audio_base64"], after["audio_base64"]);
        supervisor.stop(AiService::Tts).await.unwrap();
    }

    #[tokio::test]
    async fn python_plugin_host_roundtrip() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let python = test_python();
        let config = PythonHostConfig {
            python_executable: python.into(),
            sdk_root: root.clone(),
            plugins_directory: root.join("plugins/builtin"),
            models_directory: None,
            request_timeout: Duration::from_secs(10),
        };
        let supervisor = AiSupervisor::new(ProcessTopology::Split, config);
        let first = supervisor.start(AiService::Tts).await.unwrap();
        assert!(
            supervisor
                .discover(AiService::Tts)
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == "mock_tts")
        );
        assert!(
            supervisor
                .initialize(AiService::Tts, "mock_tts", json!({"speaker": "bright"}))
                .await
                .unwrap()["model_ready"]
                .as_bool()
                .unwrap()
        );
        let result = supervisor
            .call(
                AiService::Tts,
                "infer",
                json!({"kind": "tts", "input": {"text": "Hello"}}),
            )
            .await
            .unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(result["audio_base64"].as_str().unwrap())
            .unwrap();
        assert!(decoded.starts_with(b"RIFF"));
        assert!(
            supervisor
                .call(
                    AiService::Tts,
                    "initialize",
                    json!({"plugin_id": "mock_tts"})
                )
                .await
                .is_err()
        );
        supervisor.stop(AiService::Tts).await.unwrap();
        assert!(matches!(
            supervisor.call(AiService::Tts, "discover", json!({})).await,
            Err(AiRuntimeError::NotRunning)
        ));
        let second = supervisor.start(AiService::Tts).await.unwrap();
        assert_ne!(first, second);
        supervisor.stop(AiService::Tts).await.unwrap();
    }

    #[tokio::test]
    async fn shared_host_can_run_tts_and_asr_without_separate_processes() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
        let python = test_python();
        let supervisor = Arc::new(AiSupervisor::new(
            ProcessTopology::Shared,
            PythonHostConfig {
                python_executable: python.into(),
                sdk_root: root.clone(),
                plugins_directory: root.join("plugins/builtin"),
                models_directory: None,
                request_timeout: Duration::from_secs(10),
            },
        ));
        supervisor.start(AiService::Tts).await.unwrap();
        assert!(matches!(
            supervisor.start(AiService::Asr).await,
            Err(AiRuntimeError::AlreadyRunning)
        ));
        supervisor
            .initialize(AiService::Tts, "mock_tts", json!({}))
            .await
            .unwrap();
        supervisor
            .initialize(AiService::Asr, "mock_asr", json!({ "language": "ru" }))
            .await
            .unwrap();
        let transcript = supervisor
            .call(
                AiService::Asr,
                "infer",
                json!({ "kind": "asr", "input": { "mock_transcript": "Привет" } }),
            )
            .await
            .unwrap();
        assert_eq!(transcript["text"], "Привет");
        let voice = PythonVoiceProvider::new(Arc::clone(&supervisor));
        let audio = voice
            .synthesize(
                Arc::<str>::from("Тест"),
                VoiceOptions {
                    voice: "default".into(),
                    volume: crate::settings::Volume::try_from(100).unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(audio.format, AudioFormat::Wave);
        assert!(audio.bytes.starts_with(b"RIFF"));
        supervisor.stop(AiService::Asr).await.unwrap();
        assert!(matches!(
            supervisor.call(AiService::Tts, "discover", json!({})).await,
            Err(AiRuntimeError::NotRunning)
        ));
    }
}
