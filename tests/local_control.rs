use std::{
    future::Future,
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    game_link::GameLinkConfig,
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::UserInput,
    request::{RequestHandle, RequestId, RequestOutcome, RequestSource, RequestState},
    server::{ServerConfig, ServerHandle, ServerLifecycle, ServerSnapshot},
    settings::{
        MemorySettingsRepository, PersistenceState, SettingsApi, SettingsError, SettingsRepository,
        StoredSettings, VoiceSettings, Volume,
    },
    voice::{AudioArtifact, AudioFormat, VoiceFuture, VoiceOptions, VoiceProvider},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::{Mutex, MutexGuard, mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

static APPLICATION: Mutex<()> = Mutex::const_new(());

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), future)
        .await
        .expect("contract operation timed out")
}

struct GenerationCall {
    context: neuromita::generation::GenerationInput,
    reply: oneshot::Sender<StructuredResponse>,
}

impl GenerationCall {
    fn finish(self, text: &str) {
        self.finish_structured(StructuredResponse::spoken(text));
    }

    fn finish_structured(self, response: StructuredResponse) {
        self.reply.send(response).expect("generation still running");
    }
}

struct ControlledGeneration(mpsc::UnboundedSender<GenerationCall>);

impl GenerationProvider for ControlledGeneration {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(
        &self,
        context: neuromita::generation::GenerationInput,
        _: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(GenerationCall { context, reply })
                .map_err(|_| GenerationError::Transport("fixture stopped".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture response dropped".into()))
        })
    }
}

struct FixtureVoice;
impl VoiceProvider for FixtureVoice {
    fn synthesize(&self, _text: Arc<str>, _options: VoiceOptions) -> VoiceFuture<'_> {
        Box::pin(async {
            Ok(AudioArtifact {
                format: AudioFormat::Wave,
                bytes: Arc::from(b"RIFF-fixture-wave".as_slice()),
            })
        })
    }
}

struct FailOnDemandSettingsRepository {
    inner: MemorySettingsRepository,
    armed: AtomicBool,
    failures: AtomicUsize,
}

impl FailOnDemandSettingsRepository {
    fn new() -> Self {
        Self {
            inner: MemorySettingsRepository::new(),
            armed: AtomicBool::new(false),
            failures: AtomicUsize::new(0),
        }
    }
}

impl SettingsRepository for FailOnDemandSettingsRepository {
    fn migrate(&self) -> Result<(), SettingsError> {
        self.inner.migrate()
    }

    fn load(&self) -> Result<StoredSettings, SettingsError> {
        self.inner.load()
    }

    fn save(&self, settings: &StoredSettings) -> Result<(), SettingsError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.failures.fetch_add(1, Ordering::SeqCst);
            return Err(SettingsError::Storage(
                "injected settings write failure".into(),
            ));
        }
        self.inner.save(settings)
    }
}

struct Fixture {
    app: ApplicationLifecycle,
    calls: mpsc::UnboundedReceiver<GenerationCall>,
    _serial: MutexGuard<'static, ()>,
}

#[derive(Default)]
struct FixtureOptions {
    telegram: bool,
    remote_voice: bool,
    settings_repository: Option<Arc<dyn SettingsRepository>>,
    prompt_root: Option<std::path::PathBuf>,
}

impl Fixture {
    async fn start(configure: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::with_queue(8, configure).await
    }

    async fn with_queue(queue: usize, configure: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::with_voice(queue, configure, false).await
    }

    async fn with_voice(
        queue: usize,
        configure: impl FnOnce(&mut ServerConfig),
        enabled: bool,
    ) -> Self {
        Self::with_voice_and_hub(queue, configure, enabled, None).await
    }

    async fn with_ai_hub(root: std::path::PathBuf) -> Self {
        Self::with_voice_and_hub(8, |_| {}, false, Some(root)).await
    }

    async fn with_remote_voice() -> Self {
        Self::with_options(
            8,
            |_| {},
            false,
            None,
            FixtureOptions {
                remote_voice: true,
                ..FixtureOptions::default()
            },
        )
        .await
    }

    async fn with_telegram() -> Self {
        Self::with_options(
            8,
            |_| {},
            false,
            None,
            FixtureOptions {
                telegram: true,
                ..FixtureOptions::default()
            },
        )
        .await
    }

    async fn with_prompt_catalog(root: std::path::PathBuf) -> Self {
        Self::with_options(
            8,
            |_| {},
            false,
            None,
            FixtureOptions {
                prompt_root: Some(root),
                ..FixtureOptions::default()
            },
        )
        .await
    }

    async fn with_settings_repository(repository: Arc<dyn SettingsRepository>) -> Self {
        Self::with_options(
            8,
            |_| {},
            false,
            None,
            FixtureOptions {
                settings_repository: Some(repository),
                ..FixtureOptions::default()
            },
        )
        .await
    }

    async fn with_voice_and_hub(
        queue: usize,
        configure: impl FnOnce(&mut ServerConfig),
        enabled: bool,
        hub_root: Option<std::path::PathBuf>,
    ) -> Self {
        Self::with_options(
            queue,
            configure,
            enabled,
            hub_root,
            FixtureOptions::default(),
        )
        .await
    }

    async fn with_options(
        queue: usize,
        configure: impl FnOnce(&mut ServerConfig),
        enabled: bool,
        hub_root: Option<std::path::PathBuf>,
        options: FixtureOptions,
    ) -> Self {
        let FixtureOptions {
            telegram,
            remote_voice,
            settings_repository,
            prompt_root,
        } = options;
        let serial = APPLICATION.lock().await;
        let preset = GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::google(
                "fixture",
                "unused-fixture-key",
                GenerationSettings::new(0.5, 32, Duration::from_secs(5)).unwrap(),
            )
            .unwrap(),
        ));
        let (send, calls) = mpsc::unbounded_channel();
        let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledGeneration(send));
        let mut config = ApplicationConfig::minimal(
            preset,
            ResponseGenerator::new([provider]).unwrap(),
            RuntimeLimits {
                request_queue: positive(queue),
                discovery_buffer: positive(64),
                character_commands: positive(32),
                presenter_updates: positive(64),
                retained_finished: positive(32),
            },
            "127.0.0.1:0".parse().unwrap(),
        );
        configure(&mut config.server);
        if let Some(root) = prompt_root {
            config = config.with_prompt_catalog(neuromita::prompts::PromptCatalog::new(root));
        }
        if let Some(repository) = settings_repository {
            config.settings.repository = repository;
        }
        if let Some(root) = hub_root {
            config.ai_hub_install_root = Some(root);
        }
        if enabled {
            config.voice = Arc::new(FixtureVoice);
        }
        if telegram {
            let voice = Arc::new(neuromita::telegram_voice::DynamicTelegramVoice::default());
            config.telegram_admin = Some(Arc::new(neuromita::telegram_admin::TelegramAdmin::new(
                std::env::temp_dir().join(format!("nm-telegram-ipc-{}", Uuid::new_v4())),
                voice.clone(),
            )));
            config.voice = voice;
        }
        if remote_voice {
            config.remote_voice_store = Some(
                std::env::temp_dir()
                    .join(format!("nm-remote-voice-ipc-{}", Uuid::new_v4()))
                    .join("remote_voice.rust.json"),
            );
        }
        config.game_link = Some(GameLinkConfig::new("127.0.0.1:0".parse().unwrap()));
        let app = bounded(ApplicationLifecycle::start(config)).await.unwrap();
        bounded(SettingsApi::flush()).await.unwrap();
        if enabled {
            SettingsApi::edit::<VoiceSettings>(|voice| voice.in_game = true).unwrap();
        }
        Self {
            app,
            calls,
            _serial: serial,
        }
    }

    async fn connect(&self) -> Client {
        Client::connect(self.app.server().address()).await
    }

    async fn call(&mut self) -> GenerationCall {
        bounded(self.calls.recv())
            .await
            .expect("provider fixture closed")
    }

    async fn shutdown(self) {
        bounded(self.app.shutdown()).await.unwrap();
    }
}

fn positive(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

struct Client {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    id: Uuid,
    initial_settings: Value,
}

impl Client {
    async fn connect(address: SocketAddr) -> Self {
        let stream = bounded(TcpStream::connect(address)).await.unwrap();
        let (read, write) = stream.into_split();
        let mut client = Self {
            read: BufReader::new(read),
            write,
            id: Uuid::nil(),
            initial_settings: Value::Null,
        };
        let connected = client.next().await;
        assert_eq!(connected["type"], "connected");
        client.id = uuid(&connected["client_id"]);
        client.initial_settings = client.next().await;
        assert_eq!(client.initial_settings["type"], "settings");
        client
    }

    async fn bytes(&mut self, bytes: &[u8]) {
        bounded(self.write.write_all(bytes)).await.unwrap();
    }

    async fn send(&mut self, command: Value) {
        self.bytes(&line(command)).await;
    }

    async fn next(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(
            bounded(self.read.read_line(&mut line)).await.unwrap(),
            0,
            "unexpected EOF"
        );
        serde_json::from_str(&line).expect("server JSON frame")
    }

    async fn until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        bounded(async {
            loop {
                let frame = self.next().await;
                if predicate(&frame) {
                    return frame;
                }
            }
        })
        .await
    }

    async fn accepted(&mut self, command_id: Uuid) -> Value {
        self.until(|v| v["type"] == "accepted" && v["command_id"] == command_id.to_string())
            .await
    }

    async fn terminal(&mut self, request_id: Uuid) -> Value {
        self.until(|v| {
            v["type"] == "request_status"
                && v["request_id"] == request_id.to_string()
                && (v["state"] == "finished" || v["state"] == "cancelled")
        })
        .await
    }

    async fn error(&mut self, message: &str) -> Value {
        let frame = self.until(|v| v["type"] == "error").await;
        assert!(
            frame["error"].as_str().unwrap().contains(message),
            "{frame}"
        );
        assert!(
            frame["code"].is_string(),
            "missing stable error code: {frame}"
        );
        frame
    }

    async fn closed(&mut self) {
        bounded(async {
            loop {
                let mut line = String::new();
                match self.read.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let frame: Value = serde_json::from_str(&line).unwrap();
                        assert_eq!(
                            frame["type"], "settings",
                            "unexpected frame before close: {frame}"
                        );
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        break;
                    }
                    result => panic!("expected closed connection, got {result:?}: {line}"),
                }
            }
        })
        .await;
    }
}

fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn command(name: &str) -> Value {
    json!({"command_id": Uuid::new_v4(), "command": name})
}
fn reply(id: Uuid, character: &str, text: &str) -> Value {
    let message = CharacterId::parse(character)
        .and_then(|character| {
            UserInput::new(text.to_owned())
                .ok()
                .map(|input| ChatApi::publish_message(character, input).unwrap().0)
        })
        .unwrap_or_else(Uuid::new_v4);
    json!({"command_id": id, "command": "reply", "character": character, "message_id": message})
}
fn cancel(id: Uuid) -> Value {
    json!({"command_id": Uuid::new_v4(), "command": "cancel", "request_id": id})
}
fn uuid(value: &Value) -> Uuid {
    Uuid::parse_str(value.as_str().unwrap()).unwrap()
}
fn active(id: Uuid) -> RequestHandle {
    ChatApi::active_request(RequestId(id)).unwrap().unwrap()
}

#[tokio::test]
async fn voice_failure_keeps_the_accepted_message_identity_in_status_and_replay() {
    let mut fixture = Fixture::start(|_| {}).await;
    SettingsApi::edit::<VoiceSettings>(|voice| voice.in_launcher = true).unwrap();
    let mut client = fixture.connect().await;
    let command_id = Uuid::new_v4();
    let request = reply(command_id, "Crazy", "reply with unavailable voice");
    client.send(request.clone()).await;
    let accepted = client.accepted(command_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let handle = active(request_id);
    fixture
        .call()
        .await
        .finish("Accepted text despite voice failure");
    let final_snapshot = bounded(handle.wait()).await.unwrap();
    let RequestState::Finished(RequestOutcome::VoiceGenerationFailed { response, .. }) =
        &final_snapshot.state
    else {
        panic!("expected a committed reply with a voice failure");
    };
    assert!(response.audio.is_none());
    let terminal = client.terminal(request_id).await;
    assert_eq!(terminal["outcome"], "voice_error");
    assert_eq!(terminal["text"], response.text.as_ref());
    assert_eq!(terminal["event_id"], json!(response.event_id));
    assert_eq!(terminal["can_cancel"], false);
    assert!(terminal.get("provisional").is_none());
    assert!(terminal.get("audio").is_none());

    client.send(request).await;
    let replay = client.accepted(command_id).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.terminal(request_id).await, terminal);
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

async fn server_state(server: &ServerHandle, predicate: impl Fn(&ServerSnapshot) -> bool) {
    let mut changes = server.observe();
    bounded(async {
        loop {
            if predicate(&changes.borrow_and_update()) {
                return;
            }
            changes.changed().await.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn discovery_matches_executable_commands_and_all_control_replies_are_correlated() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    assert!(client.initial_settings.get("command_id").is_none());
    let methods = command("methods");
    client.send(methods.clone()).await;
    let catalog = client.next().await;
    assert_eq!(catalog["type"], "methods");
    assert_eq!(catalog["command_id"], methods["command_id"]);
    let expected = [
        "ping",
        "methods",
        "server_status",
        "presenter_action",
        "presenter_admin_read",
        "presenter_admin_write",
        "presenter_conversation",
        "presenter_inspect",
        "game_link_inspect",
        "debug_inspect",
        "publish_message",
        "reply",
        "react",
        "initiate",
        "cancel",
        "diagnostics",
        "diagnostics_recent",
        "diagnostics_errors",
        "settings",
        "set_volume",
        "set_active_preset",
    ];
    assert_eq!(catalog["methods"], json!(expected));
    for name in expected {
        let mut cmd = command(name);
        match name {
            "reply" => {
                cmd = reply(uuid(&cmd["command_id"]), "Crazy", "catalog execution");
            }
            "publish_message" => {
                cmd["event_id"] = json!(Uuid::new_v4());
                cmd["occurred_at_unix_ms"] = json!(123);
                cmd["speaker"] = json!("player");
                cmd["content"] = json!("network publication");
                cmd["seen_by"] = json!(["Crazy"]);
            }
            "react" | "initiate" => {
                cmd["character"] = json!("Crazy");
                cmd["instruction"] = json!("catalog execution");
                if name == "react" {
                    cmd["event_id"] =
                        reply(Uuid::new_v4(), "Crazy", "reaction target")["message_id"].clone();
                }
            }
            "cancel" => {
                let mut start = command("initiate");
                start["character"] = json!("Crazy");
                client.send(start.clone()).await;
                let accepted = client.accepted(uuid(&start["command_id"])).await;
                let call = fixture.call().await;
                cmd["request_id"] = accepted["request_id"].clone();
                client.send(cmd.clone()).await;
                let response = client.until(|v| v["command_id"] == cmd["command_id"]).await;
                assert_eq!(response["type"], "cancel");
                assert_eq!(response["result"], "Requested");
                client.terminal(uuid(&accepted["request_id"])).await;
                drop(call);
                continue;
            }
            "diagnostics" => {
                cmd["character"] = json!("Crazy");
                cmd["request_id"] = json!(Uuid::new_v4());
                client.send(cmd.clone()).await;
                let response = client.until(|v| v["command_id"] == cmd["command_id"]).await;
                assert_eq!(response["type"], "error");
                assert_eq!(response["code"], "diagnostics_not_found");
                continue;
            }
            "diagnostics_recent" | "diagnostics_errors" => {
                cmd["character"] = json!("Crazy");
            }
            "debug_inspect" => cmd["scope"] = json!("capabilities"),
            "presenter_action" => cmd["action"] = json!({"action":"voice_set_volume","volume":42}),
            "presenter_admin_read" => cmd["query"] = json!({"query":"settings","area":"voice"}),
            "presenter_admin_write" => {
                cmd["change"] = json!({"change":"settings","field":"voice_volume","value":42})
            }
            "presenter_conversation" => cmd["character"] = json!("Crazy"),
            "presenter_inspect" => {
                cmd["surface"] = json!("chat");
                cmd["character"] = json!("Crazy");
            }
            "set_volume" => cmd["value"] = json!(42),
            "set_active_preset" => cmd["preset_id"] = json!(0),
            _ => {}
        }
        client.send(cmd.clone()).await;
        let response = client.until(|v| v["command_id"] == cmd["command_id"]).await;
        let response_type = match name {
            "ping" => "pong",
            "reply" | "react" | "initiate" => "accepted",
            "publish_message" => "published",
            "set_volume" => "settings_applied",
            "set_active_preset" => "preset_selected",
            other => other,
        };
        assert_eq!(response["type"], response_type, "{name}: {response}");
        if matches!(name, "reply" | "react" | "initiate") {
            let id = uuid(&response["request_id"]);
            fixture.call().await.finish("catalog result");
            assert_eq!(client.terminal(id).await["text"], "catalog result");
        }
    }
    let first = command("settings");
    let mut change = command("set_volume");
    change["value"] = json!(43);
    let last = command("settings");
    let mut pipeline = line(first.clone());
    pipeline.extend(line(change.clone()));
    pipeline.extend(line(last.clone()));
    client.bytes(&pipeline).await;
    let mut responses = Vec::new();
    let mut observed = false;
    bounded(async {
        while responses.len() < 3 || !observed {
            let frame = client.next().await;
            if frame.get("command_id").is_some() {
                responses.push(frame);
            } else if frame["type"] == "settings" && frame["settings"]["voice"]["volume"] == 43 {
                observed = true;
            }
        }
    })
    .await;
    assert_eq!(responses[0]["command_id"], first["command_id"]);
    assert_eq!(responses[0]["settings"]["voice"]["volume"], 42);
    assert_eq!(responses[1]["command_id"], change["command_id"]);
    assert_eq!(responses[1]["type"], "settings_applied");
    assert_eq!(responses[2]["command_id"], last["command_id"]);
    assert_eq!(responses[2]["settings"]["voice"]["volume"], 43);
    let instant = json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"});
    client.send(instant.clone()).await;
    fixture.call().await.finish("immediate result");
    let accepted = client.until(|v| v["type"] != "settings").await;
    assert_eq!(
        accepted["type"], "accepted",
        "status must follow acceptance"
    );
    assert_eq!(accepted["command_id"], instant["command_id"]);
    let finished = client.terminal(uuid(&accepted["request_id"])).await;
    client.send(instant.clone()).await;
    let replay = client.until(|v| v["type"] != "settings").await;
    assert_eq!(replay["type"], "accepted");
    assert_eq!(replay["command_id"], instant["command_id"]);
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.next().await, finished);
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn errors_have_stable_codes_recoverable_ids_and_no_mutations() {
    let mut fixture = Fixture::start(|s| s.receipt_capacity = positive(1)).await;
    let mut client = fixture.connect().await;
    let id = Uuid::new_v4();
    let cases = [
        (b"{\n".to_vec(), "malformed_json", None),
        (line(json!({"command_id": id, "command":"unknown"})), "unknown_command", Some(id)),
        (line(json!({"command_id": id, "command":"set_volume", "value":101})), "invalid_command", Some(id)),
        (line(json!({"command_id": id, "command":"ping", "extra":1})), "invalid_command", Some(id)),
        (line(reply(id, "Crazy", " ")), "invalid_value", Some(id)),
        (line(json!({"command_id": id, "command":"cancel", "request_id":Uuid::new_v4()})), "ownership_denied", Some(id)),
        (format!("{{\"command_id\":\"{id}\",\"command_id\":\"{id}\",\"command\":\"set_volume\",\"value\":10}}\n").into_bytes(), "invalid_command", None),
        (format!("{{\"command_id\":\"{id}\",\"command\":\"set_volume\",\"value\":10,\"value\":11}}\n").into_bytes(), "invalid_command", Some(id)),
        (format!("{{\"command_id\":\"{id}\",\"command\":\"ping\",\"command\":\"settings\"}}\n").into_bytes(), "invalid_command", Some(id)),
        (line(json!({"command_id":"bad", "command":"ping"})), "invalid_command", None),
    ];
    let before = SettingsApi::snapshot::<VoiceSettings>().unwrap();
    for (bytes, code, correlation) in cases {
        client.bytes(&bytes).await;
        let frame = client.next().await;
        assert_eq!(frame["type"], "error", "{bytes:?}");
        assert_eq!(frame["code"], code, "{frame}");
        assert_eq!(frame.get("command_id").map(uuid), correlation);
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>().unwrap().revision,
            before.revision
        );
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>().unwrap().value,
            before.value
        );
        assert!(fixture.app.timeline().is_empty());
        assert!(fixture.calls.try_recv().is_err());
        let ping = command("ping");
        client.send(ping.clone()).await;
        assert_eq!(
            client.next().await,
            json!({"type":"pong", "command_id":ping["command_id"]})
        );
    }
    let start = reply(id, "Crazy", "held");
    client.send(start).await;
    let accepted = client.accepted(id).await;
    let call = fixture.call().await;
    let conflicting = json!({"command_id":id, "command":"ping"});
    client.send(conflicting).await;
    let error = client.error("different intent").await;
    assert_eq!(error["code"], "command_id_conflict");
    assert_eq!(error["command_id"], id.to_string());
    let full = json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"});
    client.send(full.clone()).await;
    let error = client.error("too many tracked").await;
    assert_eq!(error["code"], "receipt_capacity");
    assert_eq!(error["command_id"], full["command_id"]);
    call.finish("finished");
    client.terminal(uuid(&accepted["request_id"])).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn diagnostics_report_actual_listener_clients_limits_and_sanitized_error_state() {
    let fixture = Fixture::start(|s| {
        s.max_clients = positive(3);
        s.receipt_capacity = positive(2);
    })
    .await;
    let server = fixture.app.server().clone();
    let mut client = fixture.connect().await;
    let second = fixture.connect().await;
    server_state(&server, |s| s.connected_clients == 2).await;
    {
        let cmd = command("server_status");
        client.send(cmd.clone()).await;
        let frame = client.next().await;
        assert_eq!(frame["type"], "server_status");
        assert_eq!(frame["command_id"], cmd["command_id"]);
        assert_eq!(frame["address"], server.address().to_string());
        assert_eq!(frame["running"], true);
        assert_eq!(frame["connected_clients"], 2);
        assert_eq!(frame["accepted_connections"], 2);
        assert_eq!(
            frame["limits"],
            json!({"max_clients":3,"receipt_capacity":2,"outbound_capacity":64,"max_command_bytes":65536})
        );
        assert_eq!(frame["has_error"], false);
        assert!(frame.get("last_error").is_none());
    }
    drop(second);
    server_state(&server, |s| s.connected_clients == 1).await;
    client.send(command("server_status")).await;
    let frame = client.next().await;
    assert_eq!(frame["connected_clients"], 1);
    assert_eq!(frame["accepted_connections"], 2);
    let mut invalid = fixture.connect().await;
    invalid.bytes(&[0xff, b'\n']).await;
    invalid.closed().await;
    server_state(&server, |s| {
        s.connected_clients == 1 && s.last_error.is_some()
    })
    .await;
    client.send(command("server_status")).await;
    let frame = client.next().await;
    assert_eq!(frame["has_error"], true);
    assert!(frame.get("last_error").is_none());
    assert_eq!(frame["accepted_connections"], 3);
    assert!(!frame.to_string().contains("unused-fixture-key"));
    assert!(frame.get("ready").is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn rejected_commands_are_atomic_and_connection_recovers() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut invalid = vec![
        b"{\n".to_vec(),
        b"test\n".to_vec(),
        line(command("test")),
        b"[]\n".to_vec(),
        b"null\n".to_vec(),
        b"\n".to_vec(),
        line(json!({"command":"settings"})),
        line(json!({"command_id":"invalid", "command":"initiate"})),
        line(command("unknown")),
    ];
    for name in [
        "ping",
        "methods",
        "server_status",
        "settings",
        "reply",
        "cancel",
        "set_volume",
    ] {
        let mut value = command(name);
        value["extra"] = json!(true);
        match name {
            "reply" => {
                value["character"] = json!("Crazy");
                value["text"] = json!("must not run");
            }
            "cancel" => value["request_id"] = json!(Uuid::new_v4()),
            "set_volume" => value["value"] = json!(12),
            _ => {}
        }
        invalid.push(line(value));
    }
    for value in [
        json!(-1),
        json!(101),
        json!(256),
        json!(0.5),
        json!("50"),
        Value::Null,
    ] {
        let mut cmd = command("set_volume");
        cmd["value"] = value;
        invalid.push(line(cmd));
    }
    invalid.extend([
        line(reply(Uuid::new_v4(), "missing", "hello")),
        line(reply(Uuid::new_v4(), "Crazy", " \t")),
        line(
            json!({"command_id":Uuid::new_v4(), "command":"reply", "character":"Crazy", "text":4}),
        ),
        line(json!({"command_id":Uuid::new_v4(), "command":"cancel", "request_id":"bad"})),
    ]);
    for bytes in invalid {
        let before = SettingsApi::snapshot::<VoiceSettings>().unwrap();
        let timeline = fixture.app.timeline().len();
        client.bytes(&bytes).await;
        assert_eq!(client.next().await["type"], "error", "input: {bytes:?}");
        client.send(command("settings")).await;
        let current = client.next().await;
        assert_eq!(current["type"], "settings");
        assert_eq!(current["revision"], before.revision);
        assert_eq!(
            current["settings"]["voice"]["volume"],
            before.value.volume.get()
        );
        assert_eq!(fixture.app.timeline().len(), timeline);
        assert!(fixture.calls.try_recv().is_err());
    }
    for volume in [0, 100] {
        let mut set = command("set_volume");
        set["value"] = json!(volume);
        client.send(set).await;
        let applied = client.until(|v| v["type"] == "settings_applied").await;
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>().unwrap().revision,
            applied["revision"].as_u64().unwrap()
        );
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>()
                .unwrap()
                .value
                .volume
                .get(),
            volume
        );
    }
    let id = Uuid::new_v4();
    client.send(reply(id, "Kind", "valid after errors")).await;
    let accepted = client.accepted(id).await;
    let call = fixture.call().await;
    assert_eq!(call.context.character_id, CharacterId::Kind);
    assert!(call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "valid after errors")));
    call.finish("accepted");
    assert_eq!(
        client.terminal(uuid(&accepted["request_id"])).await["text"],
        "accepted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn fragmented_utf8_and_coalesced_lines_preserve_commands() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let id = Uuid::new_v4();
    let bytes = line(
        json!({"command_id":id,"command":"initiate","character":"Crazy","instruction":"Привет"}),
    );
    let split = bytes.iter().position(|byte| *byte >= 128).unwrap() + 1;
    client.bytes(&bytes[..split]).await;
    SettingsApi::edit::<VoiceSettings>(|voice| voice.volume = Volume::new(37).unwrap()).unwrap();
    let settings = client
        .until(|v| v["type"] == "settings" && v["settings"]["voice"]["volume"] == 37)
        .await;
    assert_eq!(settings["type"], "settings");
    assert_eq!(settings["settings"]["voice"]["volume"], 37);
    assert!(fixture.calls.try_recv().is_err());
    let mut rest = bytes[split..bytes.len() - 1].to_vec();
    rest.extend_from_slice(b"\r\n");
    rest.extend_from_slice(&line(command("settings")));
    rest.extend_from_slice(&line(reply(Uuid::new_v4(), "Crazy", "test")));
    client.bytes(&rest).await;
    let accepted = client.accepted(id).await;
    let request = uuid(&accepted["request_id"]);
    let call = fixture.call().await;
    assert_eq!(&*call.context.current_input, "Привет");
    assert_eq!(
        active(request).snapshot().metadata.source,
        RequestSource::Client(client.id)
    );
    let settings = client.until(|v| v["type"] == "settings").await;
    assert_eq!(settings["settings"]["voice"]["volume"], 37);
    let second = client.until(|v| v["type"] == "accepted").await;
    assert_ne!(accepted["request_id"], second["request_id"]);
    call.finish("first");
    client.terminal(request).await;
    let call = fixture.call().await;
    assert!(call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "test")));
    call.finish("second");
    client.terminal(uuid(&second["request_id"])).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn receipts_replay_terminal_results_and_evict_any_terminal_entry() {
    let mut fixture = Fixture::start(|server| server.receipt_capacity = positive(2)).await;
    let mut client = fixture.connect().await;
    let first = Uuid::new_v4();
    let first_command = reply(first, "Crazy", "long running");
    client.send(first_command.clone()).await;
    let original = client.accepted(first).await;
    let first_request = uuid(&original["request_id"]);
    let first_call = fixture.call().await;
    client.send(first_command.clone()).await;
    let replay = client.accepted(first).await;
    assert_eq!(replay["request_id"], original["request_id"]);
    assert_eq!(replay["replayed"], true);
    for changed in [
        reply(first, "Crazy", "changed"),
        reply(first, "Kind", "long running"),
        json!({"command_id": first, "command":"set_volume", "value":1}),
    ] {
        client.send(changed).await;
        client.error("different intent").await;
    }
    assert_eq!(
        SettingsApi::snapshot::<VoiceSettings>()
            .unwrap()
            .value
            .volume
            .get(),
        80
    );
    let second = Uuid::new_v4();
    let second_command = reply(second, "Kind", "short running");
    client.send(second_command.clone()).await;
    let accepted = client.accepted(second).await;
    let second_request = uuid(&accepted["request_id"]);
    let second_call = fixture.call().await;
    client.send(reply(Uuid::new_v4(), "Kind", "full")).await;
    client.error("too many tracked requests").await;
    assert!(fixture.calls.try_recv().is_err());
    second_call.finish("terminal result");
    let finished = client.terminal(second_request).await;
    assert_eq!(finished["outcome"], "success");
    client.send(reply(Uuid::new_v4(), "Kind", " ")).await;
    client.error("target event is not observable").await;
    client.send(second_command).await;
    let replay = client.accepted(second).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.terminal(second_request).await, finished);
    assert!(fixture.calls.try_recv().is_err());
    let third = Uuid::new_v4();
    client
        .send(reply(third, "Kind", "reuse terminal slot"))
        .await;
    let accepted = client.accepted(third).await;
    fixture.call().await.finish("third result");
    client.terminal(uuid(&accepted["request_id"])).await;
    client.send(first_command).await;
    assert_eq!(
        client.accepted(first).await["request_id"],
        original["request_id"]
    );
    client.send(cancel(first_request)).await;
    let cancelled = client.until(|v| v["type"] == "cancel").await;
    assert_eq!(cancelled["result"], "Requested");
    assert_eq!(client.terminal(first_request).await["state"], "cancelled");
    drop(first_call);
    fixture.shutdown().await;
}

#[tokio::test]
async fn cancellation_and_reconnect_scopes_are_per_connection() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut owner = fixture.connect().await;
    let mut other = fixture.connect().await;
    let owner_id = owner.id;
    assert_ne!(owner.id, other.id);
    let command_id = Uuid::new_v4();
    let original_command = reply(command_id, "Crazy", "survives disconnect");
    owner.send(original_command.clone()).await;
    let accepted = owner.accepted(command_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let handle = active(request_id);
    let call = fixture.call().await;
    other.send(cancel(request_id)).await;
    other.error("does not belong").await;
    assert!(!handle.snapshot().state.is_terminal());
    let other_command = Uuid::new_v4();
    other
        .send(reply(other_command, "Kind", "other client"))
        .await;
    let other_accepted = other.accepted(other_command).await;
    let other_id = uuid(&other_accepted["request_id"]);
    let other_call = fixture.call().await;
    owner.send(cancel(other_id)).await;
    owner.error("does not belong").await;
    drop(owner);
    server_state(fixture.app.server(), |s| s.connected_clients == 1).await;
    let mut reconnected = fixture.connect().await;
    assert_ne!(reconnected.id, owner_id);
    reconnected.send(cancel(request_id)).await;
    reconnected.error("does not belong").await;
    assert_eq!(
        handle.snapshot().metadata.source,
        RequestSource::Client(owner_id)
    );
    reconnected.send(original_command).await;
    let new_accepted = reconnected.accepted(command_id).await;
    assert_eq!(new_accepted["replayed"], false);
    assert_ne!(new_accepted["request_id"], accepted["request_id"]);
    let new_id = uuid(&new_accepted["request_id"]);
    let new_handle = active(new_id);
    assert_eq!(
        new_handle.snapshot().metadata.source,
        RequestSource::Client(reconnected.id)
    );
    call.finish("completed after disconnect");
    let terminal = bounded(handle.wait()).await.unwrap();
    assert!(
        matches!(&terminal.state, RequestState::Finished(RequestOutcome::Success(response)) if &*response.text == "completed after disconnect")
    );
    fixture.call().await.finish("new connection result");
    let final_frame = reconnected
        .until(|v| {
            if v["type"] == "request_status" {
                assert_ne!(
                    v["request_id"],
                    request_id.to_string(),
                    "old request observation leaked across reconnect"
                );
            }
            v["type"] == "request_status"
                && v["request_id"] == new_id.to_string()
                && v["state"] == "finished"
        })
        .await;
    assert_eq!(final_frame["text"], "new connection result");
    assert_eq!(
        fixture
            .app
            .presenter()
            .characters
            .state(CharacterId::Crazy)
            .await
            .unwrap()
            .accepted_replies,
        2
    );
    other.send(cancel(other_id)).await;
    assert_eq!(
        other.until(|v| v["type"] == "cancel").await["result"],
        "Requested"
    );
    assert_eq!(other.terminal(other_id).await["state"], "cancelled");
    drop(other_call);
    fixture.shutdown().await;
}

#[tokio::test]
async fn queue_full_rejection_does_not_reserve_a_command_id() {
    let mut fixture = Fixture::with_queue(1, |_| {}).await;
    let mut client = fixture.connect().await;
    let first = Uuid::new_v4();
    client.send(reply(first, "Crazy", "running")).await;
    let first_request = uuid(&client.accepted(first).await["request_id"]);
    let first_call = fixture.call().await;
    let second = Uuid::new_v4();
    client.send(reply(second, "Crazy", "queued")).await;
    let second_request = uuid(&client.accepted(second).await["request_id"]);
    assert!(matches!(
        active(second_request).snapshot().state,
        RequestState::Pending
    ));
    let third = Uuid::new_v4();
    let third_command = reply(third, "Crazy", "retry after queue full");
    let timeline = fixture.app.timeline().len();
    client.send(third_command.clone()).await;
    let rejected = client.until(|v| v["type"] == "error").await;
    assert_eq!(rejected["command_id"], third.to_string());
    assert_eq!(rejected["error"], "request queue is full");
    assert_eq!(rejected["code"], "queue_full");
    assert!(fixture.calls.try_recv().is_err());
    assert_eq!(fixture.app.timeline().len(), timeline);
    first_call.finish("first done");
    client.terminal(first_request).await;
    let second_call = fixture.call().await;
    assert!(second_call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "queued")));
    client.send(third_command.clone()).await;
    let accepted = client.accepted(third).await;
    assert_eq!(accepted["replayed"], false);
    let third_request = uuid(&accepted["request_id"]);
    second_call.finish("second done");
    client.terminal(second_request).await;
    let third_call = fixture.call().await;
    assert!(third_call.context.history.events.iter().any(|event| matches!(&event.content, neuromita::timeline::EventContent::PlayerMessage { text } if &**text == "retry after queue full")));
    third_call.finish("third done");
    let finished = client.terminal(third_request).await;
    client.send(third_command).await;
    assert_eq!(
        client.accepted(third).await["request_id"],
        accepted["request_id"]
    );
    assert_eq!(client.terminal(third_request).await, finished);
    assert_eq!(
        fixture
            .app
            .presenter()
            .characters
            .state(CharacterId::Crazy)
            .await
            .unwrap()
            .accepted_replies,
        3
    );
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn max_clients_rejects_overflow_and_recovers_freed_slots() {
    let fixture = Fixture::start(|s| s.max_clients = positive(1)).await;
    for count in 1..=8 {
        let mut client = fixture.connect().await;
        server_state(fixture.app.server(), |s| s.connected_clients == 1).await;
        let rejected = bounded(TcpStream::connect(fixture.app.server().address()))
            .await
            .unwrap();
        let (read, write) = rejected.into_split();
        let mut rejected = Client {
            read: BufReader::new(read),
            write,
            id: Uuid::nil(),
            initial_settings: Value::Null,
        };
        rejected.closed().await;
        assert_eq!(fixture.app.server().snapshot().accepted_connections, count);
        client.send(command("settings")).await;
        assert_eq!(client.next().await["type"], "settings");
        drop(client);
        server_state(fixture.app.server(), |s| s.connected_clients == 0).await;
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn invalid_frames_close_connections_and_shutdown_interrupts_partial_reads() {
    const LIMIT: usize = 256;
    let fixture = Fixture::start(|s| s.max_command_bytes = positive(LIMIT)).await;
    let mut client = fixture.connect().await;
    let mut exact = line(command("settings"));
    exact.pop();
    exact.resize(LIMIT - 1, b' ');
    exact.push(b'\n');
    client.bytes(&exact).await;
    assert_eq!(client.next().await["type"], "settings");
    drop(client);
    server_state(fixture.app.server(), |s| s.connected_clients == 0).await;
    for (bytes, incomplete, error) in [
        (vec![b'x'; LIMIT + 1], false, "command too long"),
        (vec![0xff, b'\n'], false, "command is not UTF-8"),
        (
            line(command("set_volume")).into_iter().take(10).collect(),
            true,
            "incomplete command",
        ),
    ] {
        let mut client = fixture.connect().await;
        client.bytes(&bytes).await;
        if incomplete {
            bounded(client.write.shutdown()).await.unwrap();
        }
        client.closed().await;
        server_state(fixture.app.server(), |s| {
            s.connected_clients == 0
                && s.last_error
                    .as_deref()
                    .is_some_and(|message| message.contains(error))
        })
        .await;
        assert_eq!(
            SettingsApi::snapshot::<VoiceSettings>()
                .unwrap()
                .value
                .volume
                .get(),
            80
        );
        assert!(fixture.app.timeline().is_empty());
    }
    let mut idle = fixture.connect().await;
    let mut partial = fixture.connect().await;
    partial.bytes(b"{\"command_id\":").await;
    let server = fixture.app.server().clone();
    fixture.shutdown().await;
    assert!(!server.snapshot().running);
    assert_eq!(server.snapshot().connected_clients, 0);
    idle.closed().await;
    partial.closed().await;
    assert!(TcpStream::connect(server.address()).await.is_err());
}

#[tokio::test]
async fn game_link_push_resume_idempotency_and_session_ownership() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut receive = String::new();
    let hello = Uuid::new_v4();
    write
        .write_all(&line(json!({"command_id":hello,"command":"hello"})))
        .await
        .unwrap();
    reader.read_line(&mut receive).await.unwrap();
    let response: Value = serde_json::from_str(&receive).unwrap();
    assert_eq!(response["type"], "hello_ack");
    let session_id = uuid(&response["session_id"]);
    let token = uuid(&response["session_token"]);
    let issued = Uuid::new_v4();
    let command = json!({"command_id":issued,"command":"initiate","character":"Crazy","instruction":"from game"});
    write.write_all(&line(command.clone())).await.unwrap();
    receive.clear();
    reader.read_line(&mut receive).await.unwrap();
    let ack: Value = serde_json::from_str(&receive).unwrap();
    assert_eq!(ack["type"], "request_accepted");
    let request_id = uuid(&ack["request_id"]);
    assert_eq!(
        active(request_id).snapshot().metadata.source,
        RequestSource::Game(session_id)
    );
    let call = fixture.call().await;
    drop(reader);
    drop(write);

    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    write.write_all(&line(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session_id,"session_token":token}))).await.unwrap();
    receive.clear();
    reader.read_line(&mut receive).await.unwrap();
    let resumed: Value = serde_json::from_str(&receive).unwrap();
    assert_eq!(resumed["type"], "session_resumed");
    assert_eq!(resumed["requests"], 1);
    write.write_all(&line(command)).await.unwrap();
    let mut replay = None;
    for _ in 0..5 {
        receive.clear();
        bounded(reader.read_line(&mut receive)).await.unwrap();
        let event: Value = serde_json::from_str(&receive).unwrap();
        if event["type"] == "request_accepted" {
            replay = Some(event);
            break;
        }
    }
    let replay = replay.expect("command must replay after reconnect");
    assert_eq!(replay["replayed"], true);
    assert_eq!(uuid(&replay["request_id"]), request_id);
    assert!(fixture.calls.try_recv().is_err());
    call.finish("game result");
    let mut finished = false;
    for _ in 0..8 {
        receive.clear();
        bounded(reader.read_line(&mut receive)).await.unwrap();
        let event: Value = serde_json::from_str(&receive).unwrap();
        if event["type"] == "request_completed" {
            assert_eq!(event["request_id"], request_id.to_string());
            assert_eq!(event["text"], "game result");
            finished = true;
            break;
        }
    }
    assert!(finished);
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_rejects_unidentified_and_cross_session_requests() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = String::new();
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}),
        ))
        .await
        .unwrap();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "hello_required");
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["type"], "hello_ack");
    let session_id = uuid(&response["session_id"]);
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"cancel","request_id":Uuid::new_v4()}),
        ))
        .await
        .unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "ownership_denied");
    drop(reader);
    drop(write);
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    write.write_all(&line(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session_id,"session_token":Uuid::new_v4()}))).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "session_denied");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_semantic_world_events_are_atomic_and_replay_safe() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = String::new();
    write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&buffer).unwrap()["type"],
        "hello_ack"
    );
    let command_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let batch = json!({
        "command_id": command_id,
        "command": "publish_events",
        "events": [{
            "event_id": event_id, "kind": "unity_event", "occurred_at_unix_ms": 42,
            "actor": "Player", "observers": ["Crazy", "Kind"],
            "payload": {"name": "DoorOpened"}
        }]
    });
    let mut retry = batch.clone();
    retry["events"][0]["observers"] = json!(["Kind", "Crazy", "Kind"]);
    write.write_all(&line(retry)).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(receipt["type"], "events_published");
    assert_eq!(receipt["event_ids"], json!([event_id]));
    write.write_all(&line(batch.clone())).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(receipt["replayed"], true);
    let mut conflicting = batch.clone();
    conflicting["events"][0]["payload"]["name"] = json!("WindowOpened");
    write.write_all(&line(conflicting)).await.unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let rejection: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(rejection["code"], "command_id_conflict");

    let (read, mut replay_write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut replay_reader = BufReader::new(read);
    replay_write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    buffer.clear();
    bounded(replay_reader.read_line(&mut buffer)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&buffer).unwrap()["type"],
        "hello_ack"
    );
    let mut retry = batch;
    retry["command_id"] = json!(Uuid::new_v4());
    replay_write.write_all(&line(retry.clone())).await.unwrap();
    buffer.clear();
    bounded(replay_reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(receipt["event_ids"], json!([event_id]));
    assert_eq!(receipt["replayed"], true);
    retry["command_id"] = json!(Uuid::new_v4());
    retry["events"][0]["observers"] = json!(["Crazy"]);
    replay_write.write_all(&line(retry)).await.unwrap();
    buffer.clear();
    bounded(replay_reader.read_line(&mut buffer)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&buffer).unwrap()["code"],
        "publish_failed"
    );
    assert_eq!(fixture.app.at().through, 1);

    write
        .write_all(&line(json!({
            "command_id": Uuid::new_v4(), "command": "react", "character": "Crazy",
            "event_id": event_id, "instruction": "remember the door"
        })))
        .await
        .unwrap();
    buffer.clear();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let accepted: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(accepted["type"], "request_accepted");
    let call = fixture.call().await;
    let cause = call.context.focus.reaction.as_ref().unwrap();
    assert_eq!(cause.event_id.0, event_id);
    let world = call.context.view.world_events();
    assert_eq!(world.len(), 1);
    assert_eq!(world[0].payload["name"], "DoorOpened");
    call.finish("door acknowledged");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_group_replies_share_the_original_message_without_disclosing_private_turns() {
    async fn read(
        reader: &mut BufReader<OwnedReadHalf>,
        predicate: impl Fn(&Value) -> bool,
    ) -> Value {
        bounded(async {
            loop {
                let mut buffer = String::new();
                assert_ne!(reader.read_line(&mut buffer).await.unwrap(), 0);
                let message: Value = serde_json::from_str(&buffer).unwrap();
                if predicate(&message) {
                    return message;
                }
            }
        })
        .await
    }

    async fn call(
        reader: &mut BufReader<OwnedReadHalf>,
        write: &mut OwnedWriteHalf,
        mut command: Value,
    ) -> Value {
        let id = json!(Uuid::new_v4());
        command["command_id"] = id.clone();
        write.write_all(&line(command)).await.unwrap();
        read(reader, |frame| frame["command_id"] == id).await
    }

    let mut fixture = Fixture::start(|_| {}).await;
    let (reader, mut write) = TcpStream::connect(fixture.app.game_link().unwrap().address())
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(reader);
    assert_eq!(
        call(&mut reader, &mut write, json!({"command":"hello"})).await["type"],
        "hello_ack"
    );
    let group = json!({
        "targets":[{"character":"Crazy"},{"character":"Kind"}],
        "participants":["player",{"character":"Crazy"},{"character":"Kind"},{"character":"Cappie"}],
        "seen_by":["Crazy","Kind"]
    });
    let event = Uuid::new_v4();
    assert_eq!(
        call(
            &mut reader,
            &mut write,
            json!({
                "command":"publish_message", "character":"Crazy", "event_id":event,
                "occurred_at_unix_ms":42, "content":"Hello group", "audience":group
            })
        )
        .await["event_id"],
        json!(event)
    );

    let mut response = json!(event);
    for (character, text) in [("Crazy", "Hello Kind"), ("Kind", "Hello Crazy")] {
        let accepted = call(
            &mut reader,
            &mut write,
            json!({
                "command":"reply", "character":character, "message_id":response, "audience":group
            }),
        )
        .await;
        assert_eq!(accepted["type"], "request_accepted");
        let generation = fixture.call().await;
        assert!(
            generation
                .context
                .view
                .messages()
                .iter()
                .any(|m| json!(m.event_id.0) == response)
        );
        generation.finish(text);
        let finished = read(&mut reader, |f| {
            f["type"] == "request_completed" && f["request_id"] == accepted["request_id"]
        })
        .await;
        response = finished["event_id"].clone();
    }
    assert_eq!(fixture.app.at().through, 3);
    assert_eq!(
        call(
            &mut reader,
            &mut write,
            json!({
                "command":"reply", "character":"Cappie", "message_id":response
            })
        )
        .await["type"],
        "error"
    );

    let private = Uuid::new_v4();
    assert_eq!(call(&mut reader, &mut write, json!({
        "command":"publish_message", "character":"Crazy", "event_id":private,
        "occurred_at_unix_ms":43, "content":"Private", "audience":{
            "targets":[{"character":"Crazy"}], "participants":["player",{"character":"Crazy"}], "seen_by":["Crazy"]
        }
    })).await["type"], "published");
    assert_eq!(
        call(
            &mut reader,
            &mut write,
            json!({
                "command":"reply", "character":"Kind", "message_id":private
            })
        )
        .await["type"],
        "error"
    );
    assert_eq!(fixture.app.at().through, 4);
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_resume_replaces_old_tcp_connection() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (old_read, mut old_write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut old_reader = BufReader::new(old_read);
    let mut buffer = String::new();
    old_write
        .write_all(&line(
            json!({"command_id":Uuid::new_v4(),"command":"hello"}),
        ))
        .await
        .unwrap();
    bounded(old_reader.read_line(&mut buffer)).await.unwrap();
    let receipt: Value = serde_json::from_str(&buffer).unwrap();
    let (new_read, mut new_write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut new_reader = BufReader::new(new_read);
    new_write
        .write_all(&line(json!({
            "command_id":Uuid::new_v4(),"command":"resume",
            "session_id":receipt["session_id"],"session_token":receipt["session_token"]
        })))
        .await
        .unwrap();
    buffer.clear();
    bounded(new_reader.read_line(&mut buffer)).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&buffer).unwrap()["type"],
        "session_resumed"
    );
    buffer.clear();
    assert_eq!(bounded(old_reader.read_line(&mut buffer)).await.unwrap(), 0);
    new_write
        .write_all(&line(json!({"command_id":Uuid::new_v4(),"command":"ping"})))
        .await
        .unwrap();
    let response = bounded(async {
        loop {
            buffer.clear();
            new_reader.read_line(&mut buffer).await.unwrap();
            let event: Value = serde_json::from_str(&buffer).unwrap();
            if event["type"] == "pong" {
                break event;
            }
        }
    })
    .await;
    assert_eq!(response["type"], "pong");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_enforces_frame_size_while_receiving() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let (read, mut write) = bounded(TcpStream::connect(address))
        .await
        .unwrap()
        .into_split();
    let mut reader = BufReader::new(read);
    let mut buffer = String::new();
    write.write_all(&vec![b'X'; 100_000]).await.unwrap();
    bounded(reader.read_line(&mut buffer)).await.unwrap();
    let response: Value = serde_json::from_str(&buffer).unwrap();
    assert_eq!(response["code"], "command_too_large");
    buffer.clear();
    assert_eq!(bounded(reader.read_line(&mut buffer)).await.unwrap(), 0);
    fixture.shutdown().await;
}

struct GameClient {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}
impl GameClient {
    async fn connect(address: SocketAddr) -> Self {
        let (read, write) = bounded(TcpStream::connect(address))
            .await
            .unwrap()
            .into_split();
        Self {
            read: BufReader::new(read),
            write,
        }
    }
    async fn send(&mut self, command: Value) {
        bounded(self.write.write_all(&line(command))).await.unwrap();
    }
    async fn next(&mut self) -> Value {
        let mut frame = String::new();
        assert_ne!(bounded(self.read.read_line(&mut frame)).await.unwrap(), 0);
        serde_json::from_str(&frame).unwrap()
    }
    async fn until(&mut self, expected: &str) -> Value {
        bounded(async {
            loop {
                let frame = self.next().await;
                if frame["type"] == expected {
                    return frame;
                }
            }
        })
        .await
    }
    async fn hello(&mut self) -> (Uuid, Uuid) {
        self.send(json!({"command":"hello","command_id":Uuid::new_v4()}))
            .await;
        let answer = self.until("hello_ack").await;
        (uuid(&answer["session_id"]), uuid(&answer["session_token"]))
    }
}

#[tokio::test]
async fn game_link_delivers_one_canonical_hint_and_rejects_invalid_segment_actions() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut game = GameClient::connect(fixture.app.game_link().unwrap().address()).await;
    game.hello().await;

    for (segment, outcome) in [
        (
            json!({"text":"Here you go", "hint":" Check the console ", "intents":[
                {"type":"hint.show", "payload":{"text":"Check the console"}}
            ]}),
            "request_completed",
        ),
        (
            json!({"text":"Here you go", "intents":[
                {"type":"hint.show", "payload":{"message":" Check the console "}}
            ]}),
            "request_completed",
        ),
        (
            json!({"text":"Here you go", "hint":"Different text", "intents":[
                {"type":"hint.show", "payload":{"text":"Check the console"}}
            ]}),
            "request_failed",
        ),
        (
            json!({"text":"Here you go", "intents":[
                {"type":"hint.show", "payload":{"text":false}}
            ]}),
            "request_failed",
        ),
        (
            json!({"text":"Here you go", "hint":"Check the console", "intents":[
                {"type":"unknown.action", "payload":{}}
            ]}),
            "request_failed",
        ),
    ] {
        let previous_head = fixture.app.at().through;
        game.send(json!({"command":"initiate","command_id":Uuid::new_v4(),"character":"Crazy"}))
            .await;
        let request = game.until("request_accepted").await;
        let response =
            StructuredResponse::parse(&json!({"segments":[segment]}).to_string()).unwrap();
        fixture.call().await.finish_structured(response);
        let result = game.until(outcome).await;
        assert_eq!(result["request_id"], request["request_id"]);
        if outcome == "request_completed" {
            assert_eq!(
                result["segments"],
                json!([{"text":"Here you go","hint":"Check the console"}])
            );
            assert_eq!(fixture.app.at().through, previous_head + 1);
        } else {
            assert!(result["event_id"].is_null());
            assert_eq!(fixture.app.at().through, previous_head);
        }
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_captures_world_context_at_admission_and_restores_revision() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut game = GameClient::connect(address).await;
    let (session, token) = game.hello().await;
    let first = Uuid::new_v4();
    let context = json!({"revision":1,"scene":"Kitchen","world":{"room":"Kitchen","actors":["Crazy","Player"]},"capabilities":["look","walk"]});
    game.send(json!({"command_id":first,"command":"update_context","context":context}))
        .await;
    let ack = game.until("context_applied").await;
    assert_eq!(ack["revision"], 1);
    game.send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy","instruction":"look around"})).await;
    let accepted = game.until("request_accepted").await;
    let first_id = uuid(&accepted["request_id"]);
    let call = fixture.call().await;
    assert_eq!(
        call.context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .as_deref(),
        Some("Kitchen")
    );
    assert_eq!(call.context.runtime_context.as_ref().unwrap().revision, 1);
    assert_eq!(
        call.context.runtime_context.as_ref().unwrap().capabilities,
        vec!["look", "walk"]
    );
    game.send(json!({"command_id":Uuid::new_v4(),"command":"update_context","context":{"revision":2,"scene":"Garden","world":{},"capabilities":[]}})).await;
    assert_eq!(game.until("context_applied").await["revision"], 2);
    assert_eq!(
        call.context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .as_deref(),
        Some("Kitchen")
    );
    call.finish("first game response");
    game.send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    let second = fixture.call().await;
    assert_eq!(
        second
            .context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .as_deref(),
        Some("Garden")
    );
    second.finish("second game response");
    let cmd = Uuid::new_v4();
    game.send(json!({"command_id":cmd,"command":"update_context","context":{"revision":2,"scene":"Kitchen","world":{},"capabilities":[]}})).await;
    assert_eq!(game.until("error").await["code"], "context_rejected");
    game.send(json!({"command_id":first,"command":"update_context","context":context}))
        .await;
    assert_eq!(
        game.until("context_applied").await["command_id"],
        first.to_string()
    );
    drop(game);
    let mut resumed = GameClient::connect(address).await;
    resumed.send(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session,"session_token":token})).await;
    assert_eq!(
        resumed.until("session_resumed").await["session_id"],
        session.to_string()
    );
    let state = resumed.until("session_state").await;
    assert_eq!(state["context_revision"], 2);
    assert_ne!(first_id, Uuid::nil());
    fixture.shutdown().await;
}

#[tokio::test]
async fn lifecycle_probe_identifies_distinct_real_core_instances() {
    let first = Fixture::start(|_| {}).await;
    let mut first_client = first.connect().await;
    let query = command("debug_inspect");
    let mut command = query.clone();
    command["scope"] = json!("lifecycle");
    first_client.send(command.clone()).await;
    let before = first_client
        .until(|frame| frame["command_id"] == command["command_id"])
        .await;
    let first_id = uuid(&before["snapshot"]["core_instance_id"]);
    assert_eq!(before["snapshot"]["core_phase"], "running");
    drop(first_client);
    first.shutdown().await;

    let second = Fixture::start(|_| {}).await;
    let mut second_client = second.connect().await;
    second_client.send(command.clone()).await;
    let after = second_client
        .until(|frame| frame["command_id"] == command["command_id"])
        .await;
    let second_id = uuid(&after["snapshot"]["core_instance_id"]);
    assert_ne!(first_id, second_id);
    assert_eq!(after["snapshot"]["core_phase"], "running");
    second.shutdown().await;
}

#[tokio::test]
async fn game_link_audio_chunks_delivery_ack_and_ptt_survive_resume() {
    let mut fixture = Fixture::with_voice(8, |_| {}, true).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut game = GameClient::connect(address).await;
    let (session, token) = game.hello().await;
    let capture = Uuid::new_v4();
    let ptt_cmd = Uuid::new_v4();
    game.send(json!({"command_id":ptt_cmd,"command":"report_ptt","capture_id":capture,"generation":4,"active":true})).await;
    assert_eq!(game.until("ptt_accepted").await["generation"], 4);
    game.send(json!({"command_id":Uuid::new_v4(),"command":"report_ptt","capture_id":capture,"generation":3,"active":false})).await;
    assert_eq!(game.until("error").await["code"], "ptt_rejected");
    game.send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    let request = uuid(&game.until("request_accepted").await["request_id"]);
    fixture.call().await.finish("audio output");
    let completed = game.until("request_completed").await;
    assert_eq!(completed["request_id"], request.to_string());
    assert_eq!(completed["audio_asset"]["format"], "wave");
    assert_eq!(completed["audio_asset"]["size_bytes"], 17);
    assert!(completed["audio"].is_null());
    let event = uuid(&completed["event_id"]);
    let read_id = Uuid::new_v4();
    game.send(json!({"command_id":read_id,"command":"read_audio_chunk","request_id":request,"offset":0,"length":128})).await;
    let chunk = game.until("audio_chunk").await;
    assert_eq!(chunk["base64"], "UklGRi1maXh0dXJlLXdhdmU=");
    let playback = Uuid::new_v4();
    game.send(json!({"command_id":Uuid::new_v4(),"command":"report_playback","request_id":request,"playback_id":playback,"phase":"started"})).await;
    assert_eq!(game.until("playback_accepted").await["phase"], "started");
    game.send(json!({"command_id":Uuid::new_v4(),"command":"report_playback","request_id":request,"playback_id":playback,"phase":"completed"})).await;
    assert_eq!(game.until("playback_accepted").await["phase"], "completed");
    let ack_id = Uuid::new_v4();
    game.send(json!({"command_id":ack_id,"command":"acknowledge_result","request_id":request,"event_id":event,"applied":true})).await;
    assert_eq!(game.until("result_acknowledged").await["applied"], true);
    let mut inspector = fixture.connect().await;
    let mut inspect = command("game_link_inspect");
    inspect["session_id"] = json!(session);
    inspect["request_id"] = json!(request);
    inspector.send(inspect.clone()).await;
    let result = inspector
        .until(|frame| frame["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(result["type"], "game_link_inspect");
    assert_eq!(
        result["snapshot"]["source"],
        "game_link_protocol_session_owner"
    );
    assert_eq!(result["snapshot"]["session"]["observed"], true);
    assert_eq!(result["snapshot"]["session"]["delivery_acks"], 1);
    assert_eq!(result["snapshot"]["session"]["resume_count"], 0);
    assert_eq!(result["snapshot"]["request"]["request_id"], json!(request));
    assert_eq!(result["snapshot"]["request"]["phase"], "finished");
    assert_eq!(result["snapshot"]["request"]["event_id"], json!(event));
    assert_eq!(
        result["snapshot"]["request"]["delivery_ack"]["applied"],
        true
    );
    assert_eq!(result["snapshot"]["request"]["playback_phase"], "completed");
    assert_eq!(result["snapshot"]["full_stack_e2e_verified"], false);
    assert!(!result.to_string().contains("audio output"));
    assert!(!result.to_string().contains("RIFF-fixture-wave"));
    assert!(!result.to_string().contains(token.to_string().as_str()));

    let mut no_session = command("game_link_inspect");
    no_session["request_id"] = json!(request);
    inspector.send(no_session.clone()).await;
    let invalid = inspector
        .until(|frame| frame["command_id"] == no_session["command_id"])
        .await;
    assert_eq!(invalid["type"], "error");
    assert_eq!(invalid["code"], "invalid_value");

    let mut cross = command("game_link_inspect");
    cross["session_id"] = json!(Uuid::new_v4());
    cross["request_id"] = json!(request);
    inspector.send(cross.clone()).await;
    let hidden = inspector
        .until(|frame| frame["command_id"] == cross["command_id"])
        .await;
    assert_eq!(hidden["snapshot"]["session"]["observed"], false);
    assert_eq!(hidden["snapshot"]["request"]["observed"], false);

    drop(game);
    let mut resumed = GameClient::connect(address).await;
    resumed.send(json!({"command_id":Uuid::new_v4(),"command":"resume","session_id":session,"session_token":token})).await;
    resumed.until("session_resumed").await;
    let state = resumed.until("session_state").await;
    assert_eq!(state["ptt"]["capture_id"], capture.to_string());
    assert_eq!(state["deliveries"][0]["ack"]["applied"], true);
    assert_eq!(state["playback"][0]["report"]["phase"], "completed");
    inspector.send(inspect.clone()).await;
    let after_resume = inspector
        .until(|frame| frame["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(
        after_resume["snapshot"]["request"]["delivery_ack"]["applied"],
        true
    );
    assert_eq!(after_resume["snapshot"]["session"]["connected"], true);
    assert_eq!(after_resume["snapshot"]["session"]["resume_count"], 1);
    assert_eq!(after_resume["snapshot"]["session"]["delivery_acks"], 1);
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_restricts_audio_and_result_ack_to_session() {
    let mut fixture = Fixture::with_voice(8, |_| {}, true).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut first = GameClient::connect(address).await;
    first.hello().await;
    let mut second = GameClient::connect(address).await;
    second.hello().await;
    first
        .send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    let request = uuid(&first.until("request_accepted").await["request_id"]);
    fixture.call().await.finish("secret");
    let frame = first.until("request_completed").await;
    let event = uuid(&frame["event_id"]);
    second.send(json!({"command_id":Uuid::new_v4(),"command":"read_audio_chunk","request_id":request,"offset":0,"length":1024})).await;
    assert_eq!(second.until("error").await["code"], "ownership_denied");
    second.send(json!({"command_id":Uuid::new_v4(),"command":"acknowledge_result","request_id":request,"event_id":event,"applied":true})).await;
    assert_eq!(second.until("error").await["code"], "ownership_denied");
    first.send(json!({"command_id":Uuid::new_v4(),"command":"read_audio_chunk","request_id":request,"offset":0,"length":20_000})).await;
    assert_eq!(first.until("error").await["code"], "invalid_chunk");
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_pushes_scoped_game_settings_without_polling() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut game = GameClient::connect(address).await;
    game.hello().await;
    game.send(json!({"command_id":Uuid::new_v4(),"command":"get_game_settings"}))
        .await;
    let initial = game.until("game_settings").await;
    assert!(initial["voice"]["enabled"].is_boolean());
    let revision = initial["revision"].as_u64().unwrap();
    SettingsApi::edit::<VoiceSettings>(|voice| {
        voice.in_game = !voice.in_game;
    })
    .unwrap();
    let pushed = game.until("game_settings_updated").await;
    assert!(pushed["revision"].as_u64().unwrap() > revision);
    assert_ne!(pushed["voice"]["enabled"], initial["voice"]["enabled"]);
    assert!(pushed.get("api").is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn game_link_never_mixes_runtime_contexts_between_game_sessions() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.game_link().unwrap().address();
    let mut first = GameClient::connect(address).await;
    first.hello().await;
    let mut second = GameClient::connect(address).await;
    second.hello().await;
    first.send(json!({"command_id":Uuid::new_v4(),"command":"update_context","context":{"revision":1,"scene":"PrivateKitchen","world":{},"capabilities":[]}})).await;
    first.until("context_applied").await;
    second
        .send(json!({"command_id":Uuid::new_v4(),"command":"initiate","character":"Crazy"}))
        .await;
    second.until("request_accepted").await;
    let call = fixture.call().await;
    assert!(call.context.runtime_context.is_some());
    assert!(
        call.context
            .runtime_context
            .as_ref()
            .unwrap()
            .scene
            .is_none()
    );
    call.finish("no shared runtime scene");
    fixture.shutdown().await;
}

#[tokio::test]
async fn local_control_accepts_only_loopback_bind_addresses() {
    for address in ["0.0.0.0:0", "192.0.2.1:0", "[::]:0"] {
        let config = ServerConfig::local_control(address.parse().unwrap());
        let result = ServerLifecycle::bind(config).await;
        assert!(
            matches!(result, Err(error) if error.kind() == std::io::ErrorKind::InvalidInput),
            "LocalControl allowed {address}"
        );
    }

    let (lifecycle, handle) =
        ServerLifecycle::bind(ServerConfig::local_control("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap();
    assert!(handle.address().ip().is_loopback());
    drop(lifecycle);
}

#[tokio::test]
async fn tcp_character_to_character_reply_preserves_identity_audience_and_canonical_events() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let source_id = Uuid::new_v4();
    let publish_id = Uuid::new_v4();
    let publish = json!({
        "command_id": publish_id,
        "command": "publish_message",
        "event_id": source_id,
        "occurred_at_unix_ms": 1000,
        "speaker": {"character": "Crazy"},
        "content": "Kind, can you hear me?",
        "targets": [{"character": "Kind"}],
        "participants": [{"character": "Crazy"}, {"character": "Kind"}],
        "seen_by": ["Kind"]
    });
    client.send(publish.clone()).await;
    let published = client
        .until(|v| v["command_id"] == publish_id.to_string())
        .await;
    assert_eq!(published["type"], "published", "{published}");
    assert_eq!(published["event_id"], json!(source_id));
    assert_eq!(published["replayed"], false);
    assert_eq!(fixture.app.at().through, 1);
    let crazy = fixture
        .app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    let kind = fixture
        .app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    let cappie = fixture
        .app
        .character(CharacterId::Cappie)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    assert_eq!(crazy.view.count(), 1);
    assert_eq!(kind.view.count(), 1);
    assert_eq!(cappie.view.count(), 0);
    assert_eq!(crazy.view.messages()[0].event_id.0, source_id);
    assert_eq!(kind.view.messages()[0].event_id.0, source_id);
    assert_eq!(crazy.view.provider_messages()[0]["role"], "assistant");
    assert_eq!(kind.view.provider_messages()[0]["role"], "user");

    client.send(publish).await;
    let replay = client
        .until(|v| v["command_id"] == publish_id.to_string())
        .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(fixture.app.at().through, 1);

    let reply_id = Uuid::new_v4();
    let reply = json!({
        "command_id": reply_id,
        "command": "reply",
        "character": "Kind",
        "message_id": source_id,
        "targets": [{"character": "Crazy"}],
        "participants": [{"character": "Crazy"}, {"character": "Kind"}],
        "seen_by": ["Crazy"]
    });
    client.send(reply.clone()).await;
    let accepted = client.accepted(reply_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let call = fixture.call().await;
    assert_eq!(call.context.view.count(), 1);
    assert_eq!(call.context.view.messages()[0].event_id.0, source_id);
    assert!(call.context.current_input.is_empty());
    call.finish("Yes, I hear you.");
    let result = client.terminal(request_id).await;
    assert_eq!(result["outcome"], "success", "{result}");
    let answer_id = uuid(&result["event_id"]);
    assert_eq!(fixture.app.at().through, 2);
    let crazy = fixture
        .app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    let kind = fixture
        .app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    assert_eq!(crazy.view.count(), 2);
    assert_eq!(kind.view.count(), 2);
    assert_eq!(crazy.view.messages()[1].event_id.0, answer_id);
    assert_eq!(kind.view.messages()[1].event_id.0, answer_id);
    assert_eq!(crazy.view.provider_messages()[1]["role"], "user");
    assert_eq!(kind.view.provider_messages()[1]["role"], "assistant");
    assert_eq!(fixture.app.timeline().len(), 1);

    client.send(reply).await;
    let reply_replay = client.accepted(reply_id).await;
    assert_eq!(reply_replay["replayed"], true);
    assert_eq!(uuid(&reply_replay["request_id"]), request_id);
    assert_eq!(fixture.app.at().through, 2);
    assert!(fixture.calls.try_recv().is_err());

    let hidden_id = Uuid::new_v4();
    client
        .send(json!({
            "command_id": Uuid::new_v4(),
            "command": "publish_message",
            "event_id": hidden_id,
            "occurred_at_unix_ms": 1001,
            "speaker": "player",
            "content": "Crazy only",
            "targets": [{"character": "Kind"}],
            "participants": [{"character": "Crazy"}, {"character": "Kind"}],
            "seen_by": ["Crazy"]
        }))
        .await;
    let hidden_published = client.until(|v| v["type"] == "published").await;
    assert_eq!(hidden_published["event_id"], json!(hidden_id));
    let denied_id = Uuid::new_v4();
    client
        .send(json!({
            "command_id": denied_id,
            "command": "reply",
            "character": "Kind",
            "message_id": hidden_id
        }))
        .await;
    let denied = client
        .until(|v| v["command_id"] == denied_id.to_string())
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    assert_eq!(fixture.app.at().through, 3);
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn tcp_reply_reset_cancels_inflight_without_erasing_other_character_and_new_reply_works() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let source = Uuid::new_v4();
    let publish = Uuid::new_v4();
    client
        .send(json!({
            "command_id": publish,
            "command": "publish_message",
            "event_id": source,
            "occurred_at_unix_ms": 1000,
            "speaker": {"character": "Crazy"},
            "content": "Before Kind reset",
            "targets": [{"character": "Kind"}],
            "participants": [{"character": "Crazy"}, {"character": "Kind"}],
            "seen_by": ["Kind"]
        }))
        .await;
    assert_eq!(
        client
            .until(|frame| frame["command_id"] == publish.to_string())
            .await["type"],
        "published"
    );

    let reply_command = Uuid::new_v4();
    client
        .send(json!({
            "command_id": reply_command,
            "command": "reply",
            "character": "Kind",
            "message_id": source,
            "targets": [{"character": "Crazy"}],
            "seen_by": ["Crazy"]
        }))
        .await;
    let admitted = client.accepted(reply_command).await;
    let request = uuid(&admitted["request_id"]);
    let pending = fixture.call().await;
    assert_eq!(pending.context.view.messages()[0].event_id.0, source);

    let reset = neuromita::timeline::EventId::new();
    assert_eq!(
        fixture.app.reset_character_context(
            fixture.app.timeline_selection(),
            CharacterId::Kind,
            reset
        ),
        Ok(reset)
    );
    let cancelled = client.terminal(request).await;
    assert_eq!(cancelled["state"], "cancelled", "{cancelled}");
    assert!(
        pending
            .reply
            .send(StructuredResponse::spoken("stale reply"))
            .is_err()
    );
    assert_eq!(fixture.app.at().through, 2);

    let crazy = fixture
        .app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    let kind = fixture
        .app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    assert_eq!(crazy.view.count(), 1);
    assert_eq!(kind.view.count(), 0);
    assert_eq!(crazy.view.messages()[0].event_id.0, source);

    let new_source = Uuid::new_v4();
    let next_publish = Uuid::new_v4();
    client
        .send(json!({
            "command_id": next_publish,
            "command": "publish_message",
            "event_id": new_source,
            "occurred_at_unix_ms": 1001,
            "speaker": "player",
            "content": "After Kind reset",
            "targets": [{"character": "Kind"}],
            "seen_by": ["Kind"]
        }))
        .await;
    assert_eq!(
        client
            .until(|frame| frame["command_id"] == next_publish.to_string())
            .await["type"],
        "published"
    );

    let next_reply = Uuid::new_v4();
    client
        .send(json!({
            "command_id": next_reply,
            "command": "reply",
            "character": "Kind",
            "message_id": new_source,
            "targets": ["player"]
        }))
        .await;
    let accepted = client.accepted(next_reply).await;
    let second_request = uuid(&accepted["request_id"]);
    assert_ne!(second_request, request);
    let next_call = fixture.call().await;
    assert_eq!(next_call.context.view.count(), 1);
    assert_eq!(next_call.context.view.messages()[0].event_id.0, new_source);
    next_call.finish("Fresh reply after reset");
    let finished = client.terminal(second_request).await;
    assert_eq!(finished["outcome"], "success", "{finished}");
    assert_eq!(fixture.app.at().through, 4);
    let crazy = fixture
        .app
        .character(CharacterId::Crazy)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    let kind = fixture
        .app
        .character(CharacterId::Kind)
        .unwrap()
        .get_context(fixture.app.at())
        .await
        .unwrap();
    assert_eq!(crazy.view.count(), 1);
    assert_eq!(crazy.view.messages()[0].event_id.0, source);
    assert_eq!(kind.view.count(), 2);
    assert_eq!(kind.view.messages()[0].event_id.0, new_source);
    fixture.shutdown().await;
}

#[tokio::test]
async fn tcp_replay_treats_audience_sets_as_unordered_without_losing_conflict_detection() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let command_id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let original = json!({
        "command_id": command_id,
        "command": "publish_message",
        "event_id": event_id,
        "occurred_at_unix_ms": 777,
        "speaker": "player",
        "content": "canonical unordered audience",
        "targets": [{"character": "Crazy"}, {"character": "Kind"}],
        "participants": [{"character": "Crazy"}, {"character": "Kind"}, "player"],
        "seen_by": ["Crazy", "Kind"]
    });
    client.send(original.clone()).await;
    let published = client
        .until(|frame| frame["command_id"] == command_id.to_string())
        .await;
    assert_eq!(published["type"], "published", "{published}");
    assert_eq!(published["replayed"], false);
    assert_eq!(fixture.app.at().through, 1);
    let mut equivalent = original.clone();
    equivalent["targets"] =
        json!([{"character": "Kind"}, {"character": "Crazy"}, {"character": "Kind"}]);
    equivalent["participants"] = json!(["player", {"character": "Kind"}, {"character": "Crazy"}]);
    equivalent["seen_by"] = json!(["Kind", "Crazy", "Kind"]);
    client.send(equivalent).await;
    let replayed = client
        .until(|frame| frame["command_id"] == command_id.to_string())
        .await;
    assert_eq!(replayed["type"], "published", "{replayed}");
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["event_id"], json!(event_id));
    assert_eq!(fixture.app.at().through, 1);
    let mut changed = original;
    changed["seen_by"] = json!(["Crazy"]);
    client.send(changed).await;
    let denied = client
        .until(|frame| frame["command_id"] == command_id.to_string())
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    assert_eq!(denied["code"], "command_id_conflict");
    assert_eq!(fixture.app.at().through, 1);

    let reply_id = Uuid::new_v4();
    let reply = json!({
        "command_id": reply_id,
        "command": "reply",
        "character": "Kind",
        "message_id": event_id,
        "targets": ["player", {"character": "Crazy"}],
        "participants": [{"character": "Kind"}, {"character": "Crazy"}, "player"],
        "seen_by": ["Crazy", "Kind"]
    });
    client.send(reply.clone()).await;
    let accepted = client.accepted(reply_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let provider = fixture.call().await;
    let mut reordered_reply = reply;
    reordered_reply["targets"] = json!([{"character": "Crazy"}, "player"]);
    reordered_reply["participants"] =
        json!(["player", {"character": "Crazy"}, {"character": "Kind"}]);
    reordered_reply["seen_by"] = json!(["Kind", "Crazy", "Kind"]);
    client.send(reordered_reply).await;
    let replay = client.accepted(reply_id).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(uuid(&replay["request_id"]), request_id);
    assert!(fixture.calls.try_recv().is_err());
    provider.finish("only one reply");
    assert_eq!(client.terminal(request_id).await["outcome"], "success");
    assert_eq!(fixture.app.at().through, 2);
    fixture.shutdown().await;
}

#[tokio::test]
async fn typed_observer_visibility_and_request_probes_are_privacy_safe() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let event_id = Uuid::new_v4();
    let mut publish = command("publish_message");
    publish["event_id"] = json!(event_id);
    publish["occurred_at_unix_ms"] = json!(1234);
    publish["speaker"] = json!("player");
    publish["content"] = json!("private payload for witnesses");
    publish["targets"] = json!(["player"]);
    publish["participants"] = json!(["player"]);
    publish["seen_by"] = json!(["Crazy", "Kind"]);
    client.send(publish.clone()).await;
    let sent = client
        .until(|frame| frame["command_id"] == publish["command_id"])
        .await;
    assert_eq!(sent["type"], "published", "{sent}");

    for (observer, expected) in [("Crazy", true), ("Kind", true), ("Cappie", false)] {
        let mut cmd = command("debug_inspect");
        cmd["scope"] = json!("character");
        cmd["character"] = json!(observer);
        cmd["event_id"] = json!(event_id);
        client.send(cmd.clone()).await;
        let response = client
            .until(|frame| frame["command_id"] == cmd["command_id"])
            .await;
        assert_eq!(response["type"], "debug_inspect", "{response}");
        assert_eq!(response["snapshot"]["character"], observer);
        assert_eq!(response["snapshot"]["event_visible"], expected);
        assert_eq!(response["snapshot"]["queried_event_id"], json!(event_id));
        assert_eq!(response["snapshot"]["head_sequence"], 1);
        assert!(!response.to_string().contains("private payload"));
    }
    for bad in [
        json!({"scope":"character","event_id":event_id}),
        json!({"scope":"timeline","event_id":event_id}),
        json!({"scope":"request","character":"Crazy","request_id":Uuid::new_v4()}),
        json!({"scope":"request"}),
    ] {
        let mut cmd = command("debug_inspect");
        for (key, value) in bad.as_object().unwrap() {
            cmd[key] = value.clone();
        }
        client.send(cmd.clone()).await;
        let response = client
            .until(|frame| frame["command_id"] == cmd["command_id"])
            .await;
        assert_eq!(response["type"], "error", "{response}");
        assert_eq!(response["code"], "invalid_value");
    }

    let command_id = Uuid::new_v4();
    client
        .send(json!({"command_id":command_id,"command":"initiate","character":"Crazy"}))
        .await;
    let accepted = client.accepted(command_id).await;
    let request_id = uuid(&accepted["request_id"]);
    let provider = fixture.call().await;
    provider.finish("private model response");
    let terminal = client.terminal(request_id).await;
    assert_eq!(terminal["outcome"], "success");

    let mut observed = None;
    for _ in 0..60 {
        let mut cmd = command("debug_inspect");
        cmd["scope"] = json!("request");
        cmd["request_id"] = json!(request_id);
        client.send(cmd.clone()).await;
        let response = client
            .until(|frame| frame["command_id"] == cmd["command_id"])
            .await;
        assert_eq!(response["type"], "debug_inspect", "{response}");
        let snapshot = &response["snapshot"];
        assert_eq!(snapshot["request_id"], json!(request_id));
        assert!(!response.to_string().contains("private model response"));
        if snapshot["observed"] == true && snapshot["request"]["terminal"] == true {
            observed = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let response = observed.expect("terminal request should appear in existing ChatPresenter view");
    assert_eq!(response["snapshot"]["request"]["state"], "finished");
    assert_eq!(response["snapshot"]["request"]["outcome"], "success");
    assert!(response["snapshot"]["request"]["event_id"].is_string());
    fixture.shutdown().await;
}

#[tokio::test]
async fn debug_inspection_is_canonical_read_only_and_correlated() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    for (scope, key) in [
        ("capabilities", "protocol"),
        ("lifecycle", "core_phase"),
        ("timeline", "branch_id"),
        ("character", "available"),
        ("requests", "tracked"),
    ] {
        let mut cmd = command("debug_inspect");
        cmd["scope"] = json!(scope);
        client.send(cmd.clone()).await;
        let response = client.until(|r| r["command_id"] == cmd["command_id"]).await;
        assert_eq!(response["type"], "debug_inspect", "{response}");
        assert_eq!(response["scope"], scope);
        assert_eq!(response["evidence_level"], "core_ipc_only");
        assert!(!response["snapshot"][key].is_null(), "{response}");
    }
    let mut cmd = command("debug_inspect");
    cmd["scope"] = json!("character");
    cmd["character"] = json!("Crazy");
    client.send(cmd.clone()).await;
    let response = client.until(|r| r["command_id"] == cmd["command_id"]).await;
    assert_eq!(response["snapshot"]["character"], "Crazy");
    assert!(response["snapshot"]["visible_messages"].is_number());
    assert!(response["snapshot"].get("content").is_none());
    let mut unknown = command("debug_inspect");
    unknown["scope"] = json!("unsafe_memory_dump");
    client.send(unknown.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == unknown["command_id"])
        .await;
    assert_eq!(rejected["type"], "error");
    assert_eq!(rejected["code"], "invalid_command");
    fixture.shutdown().await;
}

#[tokio::test]
async fn agent_ipc_client_runs_real_core_probes_without_browser_or_write_access() {
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.server().address().to_string();
    neuromita::agent_ipc::run([
        "--addr".to_owned(),
        address.clone(),
        "--expect".to_owned(),
        json!({"type":"debug_inspect","snapshot":{"selection_matches_head":true}}).to_string(),
        "inspect".to_owned(),
        "timeline".to_owned(),
    ])
    .await
    .unwrap();
    neuromita::agent_ipc::run([
        "--addr".to_owned(),
        address.clone(),
        "--expect".to_owned(),
        json!({"type":"pong"}).to_string(),
        "call".to_owned(),
        json!({"command":"ping"}).to_string(),
    ])
    .await
    .unwrap();
    let denied = neuromita::agent_ipc::run([
        "--addr".to_owned(),
        address,
        "call".to_owned(),
        json!({"command":"initiate", "character":"Crazy"}).to_string(),
    ])
    .await;
    assert!(matches!(
        denied,
        Err(neuromita::agent_ipc::AgentError::WriteDenied(_))
    ));
    fixture.shutdown().await;
}

#[tokio::test]
async fn agent_ipc_client_waits_for_real_core_terminal_success() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.server().address().to_string();
    let task = tokio::spawn(neuromita::agent_ipc::run([
        "--addr".to_owned(),
        address,
        "--allow-write".to_owned(),
        "--timeout-ms".to_owned(),
        "4000".to_owned(),
        "--expect-terminal".to_owned(),
        json!({"state":"finished", "outcome":"success"}).to_string(),
        "call".to_owned(),
        json!({"command":"initiate", "character":"Crazy"}).to_string(),
    ]));
    fixture
        .call()
        .await
        .finish("observed actual provider completion");
    bounded(task).await.unwrap().unwrap();
    fixture.shutdown().await;
}

#[tokio::test]
async fn local_control_presets_inspect_and_select_use_real_catalog_and_revision() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut inspect = command("debug_inspect");
    inspect["scope"] = json!("presets");
    client.send(inspect.clone()).await;
    let first = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(first["type"], "debug_inspect");
    assert_eq!(first["snapshot"]["active_preset_id"], 0);
    assert_eq!(first["snapshot"]["runtime_bootstrap_preset_id"], 0);
    assert!(first["snapshot"].get("credentials").is_none());
    let revision = first["snapshot"]["settings_revision"].as_u64().unwrap();

    let mut invalid = command("set_active_preset");
    invalid["preset_id"] = json!(99999);
    client.send(invalid.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(rejected["code"], "invalid_value");

    let mut conflict = command("set_active_preset");
    conflict["preset_id"] = json!(0);
    conflict["expected_revision"] = json!(revision + 100);
    client.send(conflict.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == conflict["command_id"])
        .await;
    assert_eq!(rejected["code"], "settings_conflict");

    let mut select = command("set_active_preset");
    select["preset_id"] = json!(0);
    select["expected_revision"] = json!(revision);
    client.send(select.clone()).await;
    let selected = client
        .until(|r| r["command_id"] == select["command_id"])
        .await;
    assert_eq!(selected["type"], "preset_selected");
    assert_eq!(selected["active_preset_id"], 0);
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_admin_preset_revision_and_native_service_boundary() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut presets = command("presenter_inspect");
    presets["surface"] = json!("presets");
    client.send(presets.clone()).await;
    let snapshot = client
        .until(|r| r["command_id"] == presets["command_id"])
        .await;
    assert_eq!(snapshot["type"], "presenter_inspect", "{snapshot}");
    assert_eq!(snapshot["snapshot"]["source"], "presets_presenter");
    assert!(snapshot["snapshot"]["revision"].is_u64());
    assert!(snapshot["snapshot"].to_string().find("api_key").is_none());

    let mut native = command("presenter_admin_read");
    native["query"] = json!({"query":"native_models","refresh":false});
    client.send(native.clone()).await;
    let response = client
        .until(|r| r["command_id"] == native["command_id"])
        .await;
    assert_eq!(response["type"], "error");
    assert_eq!(response["code"], "invalid_value");

    let mut refresh = command("presenter_action");
    refresh["action"] = json!({"action":"native_refresh"});
    client.send(refresh.clone()).await;
    let response = client
        .until(|r| r["command_id"] == refresh["command_id"])
        .await;
    assert_eq!(response["type"], "error");
    assert_eq!(response["code"], "invalid_value");
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_ai_hub_preview_confirm_observes_real_owner_and_rejects_stale_plan() {
    let root = std::env::temp_dir().join(format!("nm-ipc-ai-hub-{}", Uuid::new_v4()));
    let fixture = Fixture::with_ai_hub(root.clone()).await;
    let mut client = fixture.connect().await;

    let mut inspect = command("presenter_inspect");
    inspect["surface"] = json!("ai_hub_install");
    client.send(inspect.clone()).await;
    let initial = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_inspect", "{initial}");
    assert_eq!(initial["snapshot"]["status"]["stage"], "idle");
    let revision = initial["snapshot"]["status"]["revision"].as_u64().unwrap();

    let mut preview = command("presenter_action");
    preview["action"] = json!({"action":"ai_hub_preview_install", "category":"tts",
        "id":"unknown-model", "token":910});
    client.send(preview.clone()).await;
    let response = client
        .until(|r| r["command_id"] == preview["command_id"])
        .await;
    assert_eq!(response["type"], "presenter_action", "{response}");
    assert_eq!(response["result"]["result"]["status"]["stage"], "failed");
    assert_eq!(
        response["result"]["result"]["status"]["error_code"],
        "invalid_selection"
    );

    let mut wait = command("presenter_admin_read");
    wait["query"] = json!({"query":"wait", "target":"ai_hub_install",
        "after_revision":revision,"timeout_ms":1000});
    client.send(wait.clone()).await;
    let observed = client
        .until(|r| r["command_id"] == wait["command_id"])
        .await;
    assert_eq!(observed["type"], "presenter_admin_read", "{observed}");
    assert_eq!(observed["snapshot"]["changed"], true, "{observed}");
    assert_eq!(observed["snapshot"]["status"]["can_install"], false);

    let mut confirm = command("presenter_action");
    confirm["action"] = json!({"action":"ai_hub_confirm_install", "category":"tts",
        "id":"unknown-model", "token":910});
    client.send(confirm.clone()).await;
    let denied = client
        .until(|r| r["command_id"] == confirm["command_id"])
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    assert_eq!(denied["code"], "invalid_value");

    fixture.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn agent_scenario_retains_cancel_and_replay_ownership_in_one_connection() {
    use neuromita::agent_ipc::{AgentError, run_local_control_scenario};
    let fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.server().address();
    let command_id = Uuid::new_v4();
    let steps = vec![
        json!({"command":"initiate", "character":"Crazy", "command_id":command_id}),
        json!({"command":"cancel", "request_id":"$last_request_id"}),
        json!({"command":"initiate", "character":"Crazy", "command_id":command_id}),
    ];
    let denied =
        run_local_control_scenario(address, Duration::from_secs(5), false, steps.clone()).await;
    assert!(matches!(denied, Err(AgentError::WriteDenied(_))));
    let result = bounded(run_local_control_scenario(
        address,
        Duration::from_secs(4),
        true,
        steps,
    ))
    .await
    .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    let records = result["steps"].as_array().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[1]["response"]["type"], "cancel");
    assert_eq!(records[2]["response"]["replayed"], true);
    assert_eq!(
        records[0]["response"]["request_id"],
        records[2]["response"]["request_id"]
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn agent_scenario_waits_for_terminal_and_follows_event_id() {
    use neuromita::agent_ipc::run_local_control_scenario;
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.server().address();
    let task = tokio::spawn(run_local_control_scenario(
        address,
        Duration::from_secs(4),
        true,
        vec![
            json!({"command":"debug_inspect","scope":"presets"}),
            json!({"command":"set_active_preset","preset_id":0}),
            json!({"command":"initiate","character":"Crazy","wait_terminal":true,
            "expect_terminal":{"state":"finished","outcome":"success"}}),
            json!({"command":"debug_inspect","scope":"character","character":"Crazy",
            "event_id":"$last_event_id"}),
        ],
    ));
    fixture.call().await.finish("accepted conversation event");
    let result = bounded(task).await.unwrap().unwrap();
    assert_eq!(result["ok"], true, "{result}");
    let records = result["steps"].as_array().unwrap();
    assert_eq!(records.len(), 4);
    assert_eq!(records[2]["terminal"]["outcome"], "success");
    assert_eq!(records[3]["response"]["snapshot"]["event_visible"], true);
    assert_eq!(records[2]["terminal"]["text"], "[redacted]");
    fixture.shutdown().await;
}

#[tokio::test]
async fn optional_http_gateway_rejects_untrusted_clients_and_probes_real_core() {
    let mut fixture = Fixture::start(|_| {}).await;
    let core = fixture.app.server().address();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reserved.local_addr().unwrap();
    drop(reserved);
    let token = "gateway-test-secret-12345678901234567890";
    let binary = env!("CARGO_BIN_EXE_neuromita-agent-http");
    let mut gateway = tokio::process::Command::new(binary)
        .args([
            "--listen",
            &address.to_string(),
            "--core",
            &core.to_string(),
        ])
        .env("NEUROMITA_AGENT_HTTP_TOKEN", token)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();
    let url = format!("http://{address}/v1/presenter/chat/Crazy");
    let mut ready = false;
    for _ in 0..50 {
        if let Ok(response) = client.get(&url).send().await {
            assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    assert!(ready, "HTTP gateway did not bind");
    let response = client.get(&url).bearer_auth(token).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let value = response.json::<Value>().await.unwrap();
    assert_eq!(value["surface"], "chat");
    assert_eq!(value["snapshot"]["source"], "chat_presenter_snapshot");
    let response = client
        .post(format!("http://{address}/v1/commands"))
        .bearer_auth(token)
        .json(&json!({"command":"initiate","character":"Crazy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(fixture.calls.try_recv().is_err());
    gateway.kill().await.unwrap();
    fixture.shutdown().await;
}

#[cfg(feature = "mcp")]
#[tokio::test]
async fn mcp_stdio_probes_the_same_running_core() {
    let fixture = Fixture::start(|_| {}).await;
    let mut process = tokio::process::Command::new(env!("CARGO_BIN_EXE_neuromita-agent-mcp"))
        .args([
            "--core",
            &fixture.app.server().address().to_string(),
            "--core-debug",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = process.stdin.take().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap()).lines();

    for (id, method, params) in [
        (
            Some(1),
            "initialize",
            json!({
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "local-control-test", "version": "1"}
            }),
        ),
        (None, "notifications/initialized", json!({})),
        (Some(2), "tools/list", json!({})),
        (
            Some(3),
            "tools/call",
            json!({"name":"inspect_presenter",
            "arguments":{"surface":"chat","character":"Crazy"}}),
        ),
        (
            Some(4),
            "tools/call",
            json!({"name":"inspect_core",
            "arguments":{"scope":"timeline"}}),
        ),
        (
            Some(5),
            "tools/call",
            json!({"name":"inspect_presets", "arguments":{}}),
        ),
        (
            Some(6),
            "tools/call",
            json!({"name":"run_scenario", "arguments":{"steps":[
                {"command":"ping"},{"command":"debug_inspect","scope":"presets"}
            ]}}),
        ),
        (
            Some(7),
            "tools/call",
            json!({"name":"inspect_onboarding", "arguments":{}}),
        ),
        (
            Some(8),
            "tools/call",
            json!({"name":"inspect_gamelink", "arguments":{}}),
        ),
    ] {
        let mut message = json!({"jsonrpc":"2.0","method":method,"params":params});
        if let Some(id) = id {
            message["id"] = json!(id);
        }
        bounded(input.write_all(format!("{message}\n").as_bytes()))
            .await
            .unwrap();
        if let Some(id) = id {
            let line = bounded(output.next_line()).await.unwrap().unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["id"], id);
            let result = &response["result"];
            match id {
                1 => assert_eq!(result["serverInfo"]["name"], "neuromita"),
                2 => {
                    let tools = result["tools"].as_array().unwrap();
                    assert!(
                        tools
                            .iter()
                            .any(|tool| tool["name"] == "presenter_character_preset")
                    );
                    assert!(
                        tools
                            .iter()
                            .any(|tool| tool["name"] == "presenter_character_prompt")
                    );
                    let mut names = std::collections::HashSet::new();
                    assert!(
                        tools
                            .iter()
                            .all(|tool| names.insert(tool["name"].as_str().unwrap()))
                    );
                }
                3 => {
                    let frame = &result["structuredContent"];
                    assert_eq!(frame["type"], "presenter_inspect");
                    assert_eq!(frame["snapshot"]["character"], "Crazy");
                }
                4 => assert_eq!(result["structuredContent"]["type"], "debug_inspect"),
                5 => assert_eq!(
                    result["structuredContent"]["snapshot"]["active_preset_id"],
                    0
                ),
                6 => assert_eq!(result["structuredContent"]["ok"], true),
                7 => {
                    assert!(result["structuredContent"]["snapshot"]["guide_completed"].is_boolean())
                }
                8 => {
                    assert_eq!(result["structuredContent"]["snapshot"]["enabled"], true);
                    assert_eq!(result["structuredContent"]["type"], "game_link_inspect");
                }
                _ => unreachable!(),
            }
        }
    }

    drop(input);
    assert!(bounded(process.wait()).await.unwrap().success());
    fixture.shutdown().await;
}

#[tokio::test]
async fn agent_ipc_binary_executes_jsonl_against_running_core() {
    let mut fixture = Fixture::start(|_| {}).await;
    let address = fixture.app.server().address().to_string();
    let cli = env!("CARGO_BIN_EXE_neuromita-agent");
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/agent_ipc_smoke.jsonl");
    let output = bounded(
        tokio::process::Command::new(cli)
            .args(["--addr", address.as_str(), "batch", script])
            .output(),
    )
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 4, "one JSONL response per probe");
    assert_eq!(rows[0]["type"], "pong");
    assert_eq!(rows[1]["type"], "server_status");
    assert_eq!(rows[2]["snapshot"]["selection_matches_head"], true);
    assert_eq!(
        rows[3]["snapshot"]["ownership"],
        "connection_local_receipts_only"
    );
    assert!(rows.iter().all(|row| row["command_id"].is_string()));

    let denied = bounded(
        tokio::process::Command::new(cli)
            .args([
                "--addr",
                address.as_str(),
                "call",
                r#"{"command":"initiate","character":"Crazy"}"#,
            ])
            .output(),
    )
    .await
    .unwrap();
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("--allow-write"));
    assert!(fixture.calls.try_recv().is_err());
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_ipc_projects_live_gui_presenters_without_private_payloads() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let observed_revision = fixture.app.presenter().chat.snapshot().revision;

    for (surface, character, source) in [
        ("chat", Some("Crazy"), "chat_presenter_snapshot"),
        ("ai_runtime", None, "runtime_state_presenter_snapshot"),
        ("connections", None, "connections_presenter_server_handle"),
    ] {
        let mut request = command("presenter_inspect");
        request["surface"] = json!(surface);
        if let Some(character) = character {
            request["character"] = json!(character);
        }
        client.send(request.clone()).await;
        let response = client
            .until(|row| row["command_id"] == request["command_id"])
            .await;
        assert_eq!(response["type"], "presenter_inspect", "{response}");
        assert_eq!(response["surface"], surface);
        assert_eq!(response["snapshot"]["source"], source);
        assert_eq!(response["snapshot"]["schema_version"], 1);
        assert_eq!(response["evidence_level"], "core_ipc_only");
        let text = response.to_string();
        assert!(!text.contains("unused-fixture-key"));
        assert!(!text.contains("RIFF-fixture"));
        if surface == "chat" {
            assert_eq!(response["snapshot"]["revision"], observed_revision);
            assert_eq!(response["snapshot"]["character"], "Crazy");
            assert_eq!(response["snapshot"]["requests"]["total"], 0);
        } else if surface == "ai_runtime" {
            let services = response["snapshot"]["services"].as_array().unwrap();
            assert_eq!(services.len(), 5);
            assert!(services.iter().all(|service| service["phase"].is_string()));
            assert!(
                services
                    .iter()
                    .all(|service| service.get("last_error").is_none())
            );
            assert!(
                services
                    .iter()
                    .all(|service| service.get("active_model").is_none())
            );
        } else {
            assert!(response["snapshot"]["connected_clients"].as_u64().unwrap() >= 1);
        }
    }

    let mut invalid = command("presenter_inspect");
    invalid["surface"] = json!("chat");
    client.send(invalid.clone()).await;
    let missing = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(missing["code"], "invalid_value");
    let mut invalid = command("presenter_inspect");
    invalid["surface"] = json!("arbitrary_core_memory");
    client.send(invalid.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(rejected["code"], "invalid_command");

    let mut observable = fixture.app.presenter().chat.observe();
    let mut initiate = command("initiate");
    initiate["character"] = json!("Crazy");
    client.send(initiate.clone()).await;
    let accepted = client.accepted(uuid(&initiate["command_id"])).await;
    let id = uuid(&accepted["request_id"]);
    fixture.call().await.finish("private fixture response");
    assert_eq!(client.terminal(id).await["outcome"], "success");
    bounded(async {
        loop {
            if observable
                .borrow()
                .requests
                .iter()
                .any(|r| r.metadata.id == RequestId(id) && r.snapshot.state.is_terminal())
            {
                break;
            }
            observable.changed().await.expect("presenter still running");
        }
    })
    .await;
    let view = fixture.app.presenter().chat.snapshot();
    assert!(view.revision > observed_revision);
    let mut request = command("presenter_inspect");
    request["surface"] = json!("chat");
    request["character"] = json!("Crazy");
    client.send(request.clone()).await;
    let response = client
        .until(|r| r["command_id"] == request["command_id"])
        .await;
    assert_eq!(response["snapshot"]["revision"], view.revision);
    assert_eq!(response["snapshot"]["requests"]["terminal"], 1);
    assert!(!response.to_string().contains("private fixture response"));

    neuromita::agent_ipc::run([
        "--addr".into(),
        fixture.app.server().address().to_string(),
        "--expect".into(),
        json!({"type":"presenter_inspect", "snapshot":{
            "schema_version":1,"surface":"chat","requests":{"terminal":1}
        }})
        .to_string(),
        "presenter".into(),
        "chat".into(),
        "Crazy".into(),
    ])
    .await
    .unwrap();
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_presenter_controls_use_actual_gui_owners_and_snapshot_observations() {
    let repository = Arc::new(MemorySettingsRepository::new());
    let mut fixture = Fixture::with_settings_repository(repository.clone()).await;
    SettingsApi::edit::<neuromita::settings::OnboardingSettings>(|state| {
        state.eula_accepted = true;
    })
    .unwrap();
    let mut client = fixture.connect().await;

    for (action, surface, key, value) in [
        (
            json!({"action":"voice_set_volume","volume":63}),
            "voice",
            "volume",
            json!(63),
        ),
        (
            json!({"action":"guide_set_level","level":"advanced"}),
            "guide",
            "level",
            json!("advanced"),
        ),
        (
            json!({"action":"guide_set_page","page":2}),
            "guide",
            "page",
            json!(2),
        ),
        (
            json!({"action":"guide_set_completed","completed":true}),
            "guide",
            "completed",
            json!(true),
        ),
    ] {
        let mut operation = command("presenter_action");
        operation["action"] = action;
        client.send(operation.clone()).await;
        let accepted = client
            .until(|frame| frame["command_id"] == operation["command_id"])
            .await;
        assert_eq!(accepted["type"], "presenter_action", "{accepted}");
        if surface == "guide" {
            assert_eq!(
                accepted["result"]["result"]["persisted"], true,
                "{accepted}"
            );
            let stored = repository.load().unwrap().settings.onboarding;
            let actual = match key {
                "level" => json!(stored.guide_level),
                "page" => json!(stored.guide_page),
                "completed" => json!(stored.guide_completed),
                _ => unreachable!("unexpected Guide setting"),
            };
            assert_eq!(actual, value);
        }
        let mut inspect = command("presenter_inspect");
        inspect["surface"] = json!(surface);
        client.send(inspect.clone()).await;
        let snapshot = client
            .until(|frame| frame["command_id"] == inspect["command_id"])
            .await;
        assert_eq!(snapshot["snapshot"][key], value, "{snapshot}");
    }

    let mut response = command("presenter_conversation");
    response["character"] = json!("Crazy");
    response["limit"] = json!(20);
    client.send(response.clone()).await;
    let empty = client
        .until(|frame| frame["command_id"] == response["command_id"])
        .await;
    assert_eq!(empty["type"], "presenter_conversation");
    assert_eq!(empty["snapshot"]["total_visible_messages"], 0);

    let mut operation = command("presenter_action");
    operation["action"] =
        json!({"action":"chat_reply","character":"Crazy","text":"presenter hello"});
    client.send(operation.clone()).await;
    let accepted = client.accepted(uuid(&operation["command_id"])).await;
    assert_eq!(accepted["source"], "gui_presenter");
    let request_id = uuid(&accepted["request_id"]);
    let generation = fixture.call().await;
    generation.finish("actual presenter answer");
    assert_eq!(client.terminal(request_id).await["outcome"], "success");

    let mut conversation = command("presenter_conversation");
    conversation["character"] = json!("Crazy");
    conversation["include_content"] = json!(true);
    client.send(conversation.clone()).await;
    let history = client
        .until(|frame| frame["command_id"] == conversation["command_id"])
        .await;
    assert_eq!(history["type"], "presenter_conversation", "{history}");
    assert!(
        history["snapshot"]["total_visible_messages"]
            .as_u64()
            .unwrap()
            >= 2
    );
    assert!(
        history["snapshot"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["content"] == "actual presenter answer")
    );
    assert!(
        history["snapshot"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["content"] == "presenter hello")
    );

    let mut no_text = command("presenter_conversation");
    no_text["character"] = json!("Crazy");
    client.send(no_text.clone()).await;
    let redacted = client
        .until(|frame| frame["command_id"] == no_text["command_id"])
        .await;
    assert!(!redacted.to_string().contains("actual presenter answer"));
    assert!(!redacted.to_string().contains("presenter hello"));

    let replay = operation.clone();
    client.send(replay.clone()).await;
    let receipt = client.accepted(uuid(&replay["command_id"])).await;
    assert_eq!(receipt["replayed"], true);
    assert_eq!(receipt["request_id"], accepted["request_id"]);
    assert!(fixture.calls.try_recv().is_err());

    let mut invalid = command("presenter_action");
    invalid["action"] =
        json!({"action":"audio_start_test","backend":"pulse","identity":"mic","secret":"injected"});
    client.send(invalid).await;
    assert_eq!(
        client.error("unknown field").await["code"],
        "invalid_command"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_admin_reads_settings_memory_and_history_with_versioned_mutations() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut settings = command("presenter_admin_read");
    settings["query"] = json!({"query":"settings","area":"voice"});
    client.send(settings.clone()).await;
    let read = client
        .until(|v| v["command_id"] == settings["command_id"])
        .await;
    assert_eq!(read["type"], "presenter_admin_read");
    assert_eq!(read["snapshot"]["values"]["volume"], 80);

    let mut write = command("presenter_admin_write");
    write["change"] = json!({"change":"settings","field":"voice_volume","value":55});
    client.send(write.clone()).await;
    assert_eq!(
        client
            .until(|v| v["command_id"] == write["command_id"])
            .await["type"],
        "presenter_admin_write"
    );
    assert_eq!(
        fixture.app.presenter().settings.voice.volume.get().get(),
        55
    );

    let mut visibility = command("presenter_admin_write");
    visibility["change"] = json!({"change":"settings","field":"section_visibility",
        "value":{"area":"api","visible":false}});
    client.send(visibility.clone()).await;
    let accepted = client
        .until(|r| r["command_id"] == visibility["command_id"])
        .await;
    assert_eq!(accepted["type"], "presenter_admin_write", "{accepted}");
    let mut interface = command("presenter_admin_read");
    interface["query"] = json!({"query":"settings","area":"interface"});
    client.send(interface.clone()).await;
    let current = client
        .until(|r| r["command_id"] == interface["command_id"])
        .await;
    assert_eq!(current["snapshot"]["values"]["visibility"]["api"], false);

    let mut invalid_visibility = command("presenter_admin_write");
    invalid_visibility["change"] = json!({"change":"settings","field":"section_visibility",
        "value":{"area":"unknown","visible":true}});
    client.send(invalid_visibility.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == invalid_visibility["command_id"])
        .await;
    assert_eq!(rejected["code"], "invalid_value", "{rejected}");
    assert!(
        !fixture
            .app
            .presenter()
            .settings
            .snapshot()
            .unwrap()
            .values
            .interface
            .visibility
            .is_visible(neuromita::settings::SettingsArea::Api)
    );

    let mut wait = command("presenter_admin_read");
    wait["query"] = json!({"query":"wait","target":"settings",
        "after_revision":read["snapshot"]["revision"],"timeout_ms":100});
    client.send(wait.clone()).await;
    let changed = client
        .until(|v| v["command_id"] == wait["command_id"])
        .await;
    assert_eq!(changed["type"], "presenter_admin_read");
    assert_eq!(changed["snapshot"]["changed"], true);

    let mut wait_again = command("presenter_admin_read");
    wait_again["query"] = json!({"query":"wait","target":"settings",
        "after_revision":changed["snapshot"]["revision"],"timeout_ms":10});
    client.send(wait_again.clone()).await;
    let timed = client
        .until(|v| v["command_id"] == wait_again["command_id"])
        .await;
    assert_eq!(timed["snapshot"]["timed_out"], true);

    let mut get = command("presenter_admin_read");
    get["query"] = json!({"query":"memories","character":"Crazy","include_content":false});
    client.send(get.clone()).await;
    let empty = client.until(|v| v["command_id"] == get["command_id"]).await;
    assert_eq!(empty["type"], "presenter_admin_read", "{empty}");
    let initial = empty["snapshot"]["version"].as_str().unwrap().to_string();
    assert_eq!(initial.len(), 64);
    assert_eq!(empty["snapshot"]["records"].as_array().unwrap().len(), 0);

    let mut add = command("presenter_admin_write");
    add["change"] = json!({"change":"memory","character":"Crazy","expected_version":initial,
        "commands":[{"command":"add","priority":"high","content":"admin private memory"}]});
    client.send(add.clone()).await;
    let applied = client.until(|v| v["command_id"] == add["command_id"]).await;
    assert_eq!(applied["type"], "presenter_admin_write", "{applied}");
    assert_eq!(applied["result"]["applied"], 1);
    assert_ne!(applied["result"]["version"], initial);
    client.send(get.clone()).await;
    let redacted = client.until(|v| v["command_id"] == get["command_id"]).await;
    assert_eq!(redacted["snapshot"]["records"].as_array().unwrap().len(), 1);
    assert!(!redacted.to_string().contains("admin private memory"));

    let mut stale = command("presenter_admin_write");
    stale["change"] = json!({"change":"memory","character":"Crazy","expected_version":initial,
        "commands":[{"command":"add","priority":"normal","content":"should not exist"}]});
    client.send(stale.clone()).await;
    let conflict = client
        .until(|v| v["command_id"] == stale["command_id"])
        .await;
    assert_eq!(conflict["type"], "error");
    assert!(conflict["error"].as_str().unwrap().contains("stale memory"));
    assert_eq!(
        fixture
            .app
            .presenter()
            .characters
            .memories(CharacterId::Crazy)
            .await
            .unwrap()
            .records()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .app
            .presenter()
            .characters
            .memories(CharacterId::Kind)
            .await
            .unwrap()
            .records()
            .len(),
        0
    );

    let mut history = command("presenter_admin_read");
    history["query"] = json!({"query":"history","character":"Crazy"});
    client.send(history.clone()).await;
    let before = client
        .until(|v| v["command_id"] == history["command_id"])
        .await;
    assert_eq!(before["type"], "presenter_admin_read", "{before}");
    let version = before["snapshot"]["version"].clone();

    let mut preview = command("presenter_admin_read");
    preview["query"] = json!({"query":"conversation_preview","character":"Crazy","change":{"action":"reset_context"}});
    client.send(preview.clone()).await;
    let pre = client
        .until(|v| v["command_id"] == preview["command_id"])
        .await;
    assert_eq!(pre["type"], "presenter_admin_read", "{pre}");
    assert_eq!(pre["snapshot"]["version"], version);

    let mut edit = command("presenter_admin_write");
    edit["change"] = json!({"change":"conversation","character":"Crazy","version":version,"edit":{"action":"reset_context"}});
    client.send(edit.clone()).await;
    let committed = client
        .until(|v| v["command_id"] == edit["command_id"])
        .await;
    assert_eq!(committed["type"], "presenter_admin_write", "{committed}");

    client.send(edit.clone()).await;
    let conflict = client
        .until(|v| v["command_id"] == edit["command_id"])
        .await;
    assert_eq!(conflict["type"], "error");
    assert!(
        conflict["error"]
            .as_str()
            .unwrap()
            .contains("stale conversation")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_speech_settings_share_gui_owner_and_preserve_selection_fields() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let command_action = |action: Value| {
        let mut operation = command("presenter_action");
        operation["action"] = action;
        operation
    };

    let voice =
        command_action(json!({"action":"speech_save_character_voice", "voice_id":"voice-default"}));
    client.send(voice.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == voice["command_id"])
        .await;
    assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    assert_eq!(
        fixture
            .app
            .presenter()
            .settings
            .tts
            .preferences
            .get()
            .default_voice_id,
        "voice-default"
    );

    let tts = fixture.app.presenter().settings.tts.preferences.get();
    let mut values = serde_json::to_value(tts).unwrap();
    values["enabled"] = json!(true);
    values["local_lazy_init"] = json!(false);
    values["default_voice_id"] = json!("try-to-overwrite");
    let save = command_action(json!({"action":"speech_save_tts","preferences":values}));
    client.send(save.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == save["command_id"])
        .await;
    assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    assert_eq!(
        fixture
            .app
            .presenter()
            .settings
            .tts
            .preferences
            .get()
            .default_voice_id,
        "voice-default"
    );
    assert!(
        fixture
            .app
            .presenter()
            .settings
            .tts
            .preferences
            .get()
            .enabled
    );

    let choose = command_action(json!({"action":"speech_select_model","service":"asr",
        "model":{"model_id":"demo_asr","implementation":"python"}}));
    client.send(choose.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == choose["command_id"])
        .await;
    assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    assert_eq!(
        fixture
            .app
            .presenter()
            .settings
            .asr
            .preferences
            .get()
            .recognizer_id,
        "demo_asr"
    );

    let mut asr =
        serde_json::to_value(fixture.app.presenter().settings.asr.preferences.get()).unwrap();
    asr["input_mode"] = json!("push_to_talk");
    asr["recognizer_id"] = json!("try-to-overwrite");
    let save =
        command_action(json!({"action":"speech_save_asr","preferences":asr,"language":"ru"}));
    client.send(save.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == save["command_id"])
        .await;
    assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    let asr = fixture.app.presenter().settings.asr.preferences.get();
    assert_eq!(asr.recognizer_id, "demo_asr");
    assert_eq!(asr.input_mode, "push_to_talk");
    assert_eq!(
        asr.model_languages.get("demo_asr").map(String::as_str),
        Some("ru")
    );

    let choose = command_action(
        json!({"action":"speech_select_asr_recognizer","model_id":"new_recognizer"}),
    );
    client.send(choose.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == choose["command_id"])
        .await;
    assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    assert_eq!(
        fixture
            .app
            .presenter()
            .settings
            .asr
            .preferences
            .get()
            .recognizer_id,
        "new_recognizer"
    );

    let invalid =
        command_action(json!({"action":"speech_select_model","service":"embedding","model":null}));
    client.send(invalid.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(receipt["type"], "error", "{receipt}");
    assert_eq!(receipt["code"], "invalid_value");
    assert!(!receipt.to_string().contains("voice-default"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_speech_inventory_reads_shared_ai_data_and_push_to_talk_uses_real_controller() {
    let root = std::env::temp_dir().join(format!("nm-ipc-speech-{}", Uuid::new_v4()));
    let fixture = Fixture::with_ai_hub(root.clone()).await;
    let mut client = fixture.connect().await;
    let mut inventory = command("presenter_admin_read");
    inventory["query"] = json!({"query":"speech_inventory"});
    client.send(inventory.clone()).await;
    let response = client
        .until(|r| r["command_id"] == inventory["command_id"])
        .await;
    assert_eq!(response["type"], "presenter_admin_read", "{response}");
    assert_eq!(response["snapshot"]["source"], "speech_model_catalog");
    assert!(response["snapshot"]["tts"].is_array());
    assert!(response["snapshot"]["asr"].is_array());
    assert_eq!(response["snapshot"]["model_activation_attempted"], false);
    assert!(
        !response
            .to_string()
            .contains(root.to_string_lossy().as_ref())
    );
    let mut press = command("presenter_action");
    press["action"] = json!({"action":"speech_push_to_talk","pressed":true});
    client.send(press.clone()).await;
    let response = client
        .until(|r| r["command_id"] == press["command_id"])
        .await;
    assert_eq!(response["type"], "error", "{response}");
    assert_eq!(response["code"], "invalid_value");
    fixture.shutdown().await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn mcp_tool_options_and_depth_share_settings_presenter_without_exposing_credentials() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    for action in [
        json!({"action":"tool_set_option","option":"calculator","enabled":true}),
        json!({"action":"tool_set_option","option":"enabled","enabled":true}),
        json!({"action":"tool_set_depth","depth":4}),
    ] {
        let mut cmd = command("presenter_action");
        cmd["action"] = action;
        client.send(cmd.clone()).await;
        let receipt = client.until(|r| r["command_id"] == cmd["command_id"]).await;
        assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    }
    let tools = fixture.app.presenter().settings.tools.state.get();
    assert!(tools.calculator);
    assert!(tools.enabled);
    assert!(!tools.web.search_enabled);
    assert!(!tools.web.enabled);
    assert_eq!(tools.max_depth, 4);
    let mut inspect = command("presenter_admin_read");
    inspect["query"] = json!({"query":"settings","area":"tools"});
    client.send(inspect.clone()).await;
    let report = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(report["snapshot"]["values"]["calculator"], true);
    assert_eq!(report["snapshot"]["values"]["max_depth"], 4);
    assert_eq!(
        report["snapshot"]["values"]["credentials_and_endpoints_withheld"],
        true
    );
    assert!(report["snapshot"]["values"].get("web").is_none());
    let mut requires_config = command("presenter_action");
    requires_config["action"] =
        json!({"action":"tool_set_option","option":"web_search","enabled":true});
    client.send(requires_config.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == requires_config["command_id"])
        .await;
    assert_eq!(rejected["code"], "invalid_value");
    assert!(
        !fixture
            .app
            .presenter()
            .settings
            .tools
            .state
            .get()
            .web
            .search_enabled
    );
    let mut invalid = command("presenter_action");
    invalid["action"] = json!({"action":"tool_set_depth","depth":6});
    client.send(invalid.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(rejected["code"], "invalid_value");
    assert_eq!(
        fixture.app.presenter().settings.tools.state.get().max_depth,
        4
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_speech_capture_reuses_core_owner_and_rejects_unready_asr() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut read = command("presenter_admin_read");
    read["query"] = json!({"query":"speech_transcript","include_content":false});
    client.send(read.clone()).await;
    let snapshot = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(snapshot["type"], "presenter_admin_read", "{snapshot}");
    assert_eq!(snapshot["snapshot"]["revision"], 0);
    assert_eq!(snapshot["snapshot"]["available"], false);
    assert!(!snapshot.to_string().contains("transcript_text"));

    let mut start = command("presenter_action");
    start["action"] = json!({"action":"speech_capture","enabled":true,
        "character":"Crazy","selection_revision":0});
    client.send(start.clone()).await;
    let reject = client
        .until(|r| r["command_id"] == start["command_id"])
        .await;
    assert_eq!(reject["type"], "error", "{reject}");
    assert_eq!(reject["code"], "invalid_value");
    assert!(reject["error"].as_str().unwrap().contains("ASR model"));

    let mut invalid = command("presenter_action");
    invalid["action"] = json!({"action":"speech_capture","enabled":false,"character":"Crazy"});
    client.send(invalid.clone()).await;
    let reject = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(reject["type"], "error", "{reject}");

    let mut stop = command("presenter_action");
    stop["action"] = json!({"action":"speech_capture","enabled":false});
    client.send(stop.clone()).await;
    let receipt = client
        .until(|r| r["command_id"] == stop["command_id"])
        .await;
    assert_eq!(receipt["type"], "presenter_action", "{receipt}");
    assert_eq!(receipt["result"]["result"]["enabled"], false);
    fixture.shutdown().await;
}

#[tokio::test]
async fn speech_transcript_observation_uses_the_same_ticket_fenced_hub_as_gui() {
    use neuromita::ai_runtime::speech::Transcript;
    use neuromita::audio::{ticket::SpeechStream, transcript_bridge::SpeechTranscriptHub};
    use tokio_util::sync::CancellationToken;

    let bus = std::sync::Arc::new(SpeechTranscriptHub::default());
    let mut gui = bus.subscribe();
    let mut observer = bus.observe();
    let stream = SpeechStream::new(CancellationToken::new());
    let conversation = bus.destination(CharacterId::Crazy, 12);
    let ticket = stream.ticket();
    let transcript = Transcript {
        text: "spoken text".into(),
        language: Some("ru".into()),
    };
    conversation.reply(&ticket, &transcript).await.unwrap();
    let event = gui.try_recv().unwrap();
    assert_eq!(event.revision, 1);
    assert_eq!(event.character, CharacterId::Crazy);
    assert_eq!(event.selection_revision, 12);
    assert_eq!(event.transcript, transcript);
    assert_eq!(observer.borrow_and_update().as_ref().unwrap().revision, 1);
    conversation.reply(&ticket, &transcript).await.unwrap();
    assert!(gui.try_recv().is_err());
    stream.invalidate();
    conversation.reply(&ticket, &transcript).await.unwrap();
    assert!(gui.try_recv().is_err());
    conversation
        .reply(&stream.ticket(), &transcript)
        .await
        .unwrap();
    assert_eq!(gui.try_recv().unwrap().revision, 2);
    assert_eq!(bus.latest().unwrap().revision, 2);
}

#[tokio::test]
async fn telegram_gui_and_mcp_use_one_owner_with_sanitized_status_and_wait() {
    let fixture = Fixture::with_telegram().await;
    let mut client = fixture.connect().await;

    let mut inspect = command("presenter_inspect");
    inspect["surface"] = json!("telegram_auth");
    client.send(inspect.clone()).await;
    let initial = client
        .until(|frame| frame["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_inspect", "{initial}");
    assert_eq!(initial["snapshot"]["state"]["phase"], 0);
    assert_eq!(initial["snapshot"]["state"]["revision"], 0);
    let telegram_state = &initial["snapshot"]["state"];
    assert!(telegram_state.get("api_hash").is_none());
    assert!(telegram_state.get("password").is_none());
    assert!(telegram_state.get("phone").is_none());

    let mut disconnect = command("presenter_action");
    disconnect["action"] = json!({"action":"telegram_disconnect"});
    client.send(disconnect.clone()).await;
    let done = client
        .until(|frame| frame["command_id"] == disconnect["command_id"])
        .await;
    assert_eq!(done["type"], "presenter_action", "{done}");
    assert_eq!(done["result"]["result"]["status"]["revision"], 1);

    let mut wait = command("presenter_admin_read");
    wait["query"] =
        json!({"query":"wait","target":"telegram_auth","after_revision":0,"timeout_ms":1000});
    client.send(wait.clone()).await;
    let observed = client
        .until(|frame| frame["command_id"] == wait["command_id"])
        .await;
    assert_eq!(observed["type"], "presenter_admin_read", "{observed}");
    assert_eq!(observed["snapshot"]["changed"], true);
    assert_eq!(observed["snapshot"]["status"]["revision"], 1);

    let mut refresh = command("presenter_action");
    refresh["action"] = json!({"action":"telegram_refresh_voice"});
    client.send(refresh.clone()).await;
    let refreshed = client
        .until(|frame| frame["command_id"] == refresh["command_id"])
        .await;
    assert_eq!(refreshed["type"], "presenter_action", "{refreshed}");
    assert_eq!(
        refreshed["result"]["result"]["status"]["voice_active"],
        false
    );

    let mut secret = command("presenter_action");
    secret["action"] = json!({"action":"telegram_password","password":"sensitive-2fa-password"});
    client.send(secret.clone()).await;
    let denied = client
        .until(|frame| frame["command_id"] == secret["command_id"])
        .await;
    assert_eq!(denied["code"], "invalid_value", "{denied}");
    assert!(!denied.to_string().contains("sensitive-2fa-password"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn remote_voice_gui_and_mcp_share_catalog_with_versioned_safe_responses() {
    let fixture = Fixture::with_remote_voice().await;
    let mut client = fixture.connect().await;
    let mut inspect = command("presenter_inspect");
    inspect["surface"] = json!("remote_voice");
    client.send(inspect.clone()).await;
    let initial = client
        .until(|frame| frame["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_inspect", "{initial}");
    assert_eq!(initial["snapshot"]["state"]["revision"], 0);
    assert_eq!(
        initial["snapshot"]["state"]["presets"][0]["credential_present"],
        false
    );

    let mut add = command("presenter_action");
    add["action"] = json!({"action":"remote_voice_add","expected_revision":0});
    client.send(add.clone()).await;
    let added = client
        .until(|frame| frame["command_id"] == add["command_id"])
        .await;
    assert_eq!(added["type"], "presenter_action", "{added}");
    assert_eq!(added["result"]["result"]["status"]["revision"], 1);
    assert_eq!(
        added["result"]["result"]["status"]["presets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let id = added["result"]["result"]["status"]["active_id"]
        .as_str()
        .unwrap();

    let mut save = command("presenter_action");
    save["action"] = json!({"action":"remote_voice_save","expected_revision":1,
        "preset":{"id":id,"name":"Mita voice","speed_percent":130}});
    client.send(save.clone()).await;
    let saved = client
        .until(|frame| frame["command_id"] == save["command_id"])
        .await;
    assert_eq!(saved["type"], "presenter_action", "{saved}");
    assert_eq!(saved["result"]["result"]["status"]["revision"], 2);
    assert!(!saved.to_string().contains("api_key"));

    let mut stale = command("presenter_action");
    stale["action"] = json!({"action":"remote_voice_delete","expected_revision":1});
    client.send(stale.clone()).await;
    let rejected = client
        .until(|frame| frame["command_id"] == stale["command_id"])
        .await;
    assert_eq!(rejected["code"], "invalid_value", "{rejected}");

    let mut wait = command("presenter_admin_read");
    wait["query"] =
        json!({"query":"wait","target":"remote_voice","after_revision":1,"timeout_ms":1000});
    client.send(wait.clone()).await;
    let changed = client
        .until(|frame| frame["command_id"] == wait["command_id"])
        .await;
    assert_eq!(changed["snapshot"]["changed"], true, "{changed}");
    assert_eq!(changed["snapshot"]["status"]["revision"], 2);

    let mut cancel = command("presenter_action");
    cancel["action"] = json!({"action":"remote_voice_cancel"});
    client.send(cancel.clone()).await;
    let cancelled = client
        .until(|frame| frame["command_id"] == cancel["command_id"])
        .await;
    assert_eq!(cancelled["result"]["result"]["status"]["phase"], "saved");
    assert_eq!(cancelled["result"]["result"]["status"]["revision"], 2);
    fixture.shutdown().await;
}

#[tokio::test]
async fn gui_equivalent_knowledge_and_admin_commands_reject_unapproved_actions() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut recall = command("presenter_admin_read");
    recall["query"] = json!({"query":"memory_recall","character":"Crazy",
        "memory_id":Uuid::new_v4(),"query_text":""});
    client.send(recall.clone()).await;
    let rejected = client
        .until(|v| v["command_id"] == recall["command_id"])
        .await;
    assert_eq!(rejected["type"], "error");

    let mut preview = command("presenter_admin_read");
    preview["query"] = json!({"query":"graph_preview","character":"Crazy",
        "memory_id":Uuid::new_v4(),"mode":"clear"});
    client.send(preview.clone()).await;
    let invalid = client
        .until(|v| v["command_id"] == preview["command_id"])
        .await;
    assert_eq!(invalid["type"], "error");

    let mut apply = command("presenter_admin_write");
    apply["change"] = json!({"change":"graph_apply","character":"Crazy",
        "request_id":Uuid::new_v4()});
    client.send(apply.clone()).await;
    let refused = client
        .until(|v| v["command_id"] == apply["command_id"])
        .await;
    assert_eq!(refused["type"], "error");

    let mut abort = command("presenter_admin_write");
    abort["change"] = json!({"change":"graph_abort","request_id":Uuid::new_v4()});
    client.send(abort.clone()).await;
    let no_plan = client
        .until(|v| v["command_id"] == abort["command_id"])
        .await;
    assert_eq!(no_plan["type"], "presenter_admin_write");
    assert_eq!(no_plan["result"]["aborted"], false);

    let mut guide = command("presenter_admin_write");
    guide["change"] = json!({"change":"guide_retry"});
    client.send(guide.clone()).await;
    let no_retry = client
        .until(|v| v["command_id"] == guide["command_id"])
        .await;
    assert_eq!(no_retry["type"], "error");

    let mut attach = command("presenter_admin_write");
    attach["change"] = json!({"change":"attach_file","character":"Crazy",
        "text":"note","source":"/not/an/approved/file",
        "version":{"branch_id":Uuid::new_v4(),"through":0,"generation":0}});
    client.send(attach.clone()).await;
    let restricted = client
        .until(|v| v["command_id"] == attach["command_id"])
        .await;
    assert_eq!(restricted["type"], "error");
    assert!(!restricted.to_string().contains("not/an/approved/file"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_hardware_refresh_uses_real_hardware_presenter_without_raw_errors() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut refresh = command("presenter_action");
    refresh["action"] = json!({"action":"hardware_refresh"});
    client.send(refresh.clone()).await;
    let outcome = client
        .until(|r| r["command_id"] == refresh["command_id"])
        .await;
    assert_eq!(outcome["type"], "presenter_action", "{outcome}");
    assert_eq!(outcome["result"]["result"]["action"], "hardware_refresh");
    assert_eq!(outcome["result"]["result"]["source"], "hardware_presenter");
    assert!(outcome["result"]["result"]["devices"].is_array());
    assert!(outcome["result"]["result"]["error_count"].is_number());
    assert!(outcome["result"]["result"].get("errors").is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_character_assignment_cas_preserves_other_slots_and_validates_catalog() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut read = command("presenter_admin_read");
    read["query"] = json!({"query":"character_profile","character":"Crazy"});
    client.send(read.clone()).await;
    let before = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(before["type"], "presenter_admin_read", "{before}");
    let revision = before["snapshot"]["revision"].as_u64().unwrap();
    assert_eq!(before["snapshot"]["presets"]["reply"], "active");
    assert_eq!(before["snapshot"]["presets"]["react"], "active");
    assert_eq!(before["snapshot"]["presets"]["initiate"], "active");

    let mut invalid = command("presenter_admin_write");
    invalid["change"] = json!({"change":"character_preset_assignment",
        "character":"Crazy","expected_revision":revision,
        "slot":"reply","selection":{"specific":999999}});
    client.send(invalid.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == invalid["command_id"])
        .await;
    assert_eq!(rejected["type"], "error", "{rejected}");
    assert_eq!(rejected["code"], "invalid_value", "{rejected}");

    let mut assign = command("presenter_admin_write");
    assign["change"] = json!({"change":"character_preset_assignment",
        "character":"Crazy","expected_revision":revision,
        "slot":"react","selection":{"specific":0}});
    client.send(assign.clone()).await;
    let selected = client
        .until(|r| r["command_id"] == assign["command_id"])
        .await;
    assert_eq!(selected["type"], "presenter_admin_write", "{selected}");
    assert_eq!(selected["result"]["presets"]["reply"], "active");
    assert_eq!(
        selected["result"]["presets"]["react"],
        json!({"specific":0})
    );
    assert_eq!(selected["result"]["presets"]["initiate"], "active");
    let next_revision = selected["result"]["revision"].as_u64().unwrap();
    assert!(next_revision > revision);

    client.send(assign.clone()).await;
    let stale = client
        .until(|r| r["command_id"] == assign["command_id"])
        .await;
    assert_eq!(stale["type"], "error", "{stale}");
    assert!(stale["error"].as_str().unwrap().contains("stale character"));

    let mut prompt = command("presenter_admin_write");
    prompt["change"] = json!({"change":"character_prompt_selection",
        "character":"Crazy","expected_revision":next_revision,
        "prompt_set":"does-not-exist"});
    client.send(prompt.clone()).await;
    let bad_prompt = client
        .until(|r| r["command_id"] == prompt["command_id"])
        .await;
    assert_eq!(bad_prompt["type"], "error", "{bad_prompt}");

    let mut reset = command("presenter_admin_write");
    reset["change"] = json!({"change":"character_preset_assignment",
        "character":"Crazy","expected_revision":next_revision,
        "slot":"all","selection":"active"});
    client.send(reset.clone()).await;
    let restored = client
        .until(|r| r["command_id"] == reset["command_id"])
        .await;
    assert_eq!(restored["type"], "presenter_admin_write", "{restored}");
    assert_eq!(restored["result"]["presets"]["reply"], "active");
    assert_eq!(restored["result"]["presets"]["react"], "active");
    assert_eq!(restored["result"]["presets"]["initiate"], "active");

    let mut full = command("presenter_admin_write");
    full["change"] = json!({"change":"character_profile","character":"Crazy",
        "expected_revision":restored["result"]["revision"],
        "profile":{"presets":{"reply":{"specific":999999},"react":"active","initiate":"active"},"prompt_set":null}});
    client.send(full.clone()).await;
    let denied = client
        .until(|r| r["command_id"] == full["command_id"])
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    client.send(read.clone()).await;
    let after = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(after["snapshot"]["presets"]["reply"], "active");
    assert_eq!(after["snapshot"]["presets"]["react"], "active");
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_settings_retry_rejects_when_no_failed_write_exists() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;

    let mut before = command("presenter_admin_read");
    before["query"] = json!({"query":"settings","area":"persistence"});
    client.send(before.clone()).await;
    let initial = client
        .until(|r| r["command_id"] == before["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_admin_read", "{initial}");
    assert!(
        !initial["snapshot"]["values"]["persistence"]
            .as_str()
            .unwrap()
            .starts_with("Failed")
    );

    let mut retry = command("presenter_admin_write");
    retry["change"] = json!({"change":"settings_retry"});
    client.send(retry.clone()).await;
    let rejected = client
        .until(|r| r["command_id"] == retry["command_id"])
        .await;
    assert_eq!(rejected["type"], "error", "{rejected}");
    assert_eq!(rejected["code"], "invalid_value", "{rejected}");
    assert!(
        rejected["error"]
            .as_str()
            .unwrap()
            .contains("no failed settings write")
    );

    client.send(before.clone()).await;
    let after = client
        .until(|r| r["command_id"] == before["command_id"])
        .await;
    assert_eq!(after["type"], "presenter_admin_read", "{after}");
    assert_eq!(
        after["snapshot"]["revision"],
        initial["snapshot"]["revision"]
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_settings_retry_recovers_real_failed_persistence_via_shared_owner() {
    let repository = Arc::new(FailOnDemandSettingsRepository::new());
    let fixture = Fixture::with_settings_repository(repository.clone()).await;
    let mut client = fixture.connect().await;

    repository.armed.store(true, Ordering::SeqCst);
    let mut change = command("presenter_admin_write");
    change["change"] = json!({"change":"settings","field":"voice_volume","value":37});
    client.send(change.clone()).await;
    let changed = client
        .until(|r| r["command_id"] == change["command_id"])
        .await;
    assert_eq!(changed["type"], "presenter_admin_write", "{changed}");
    let failed = bounded(async {
        loop {
            let snapshot = fixture.app.presenter().settings.snapshot().unwrap();
            if let PersistenceState::Failed { error, .. } = &snapshot.persistence {
                break error.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        failed
            .to_string()
            .contains("injected settings write failure")
    );
    assert_eq!(repository.failures.load(Ordering::SeqCst), 1);

    let mut inspect = command("presenter_admin_read");
    inspect["query"] = json!({"query":"settings","area":"persistence"});
    client.send(inspect.clone()).await;
    let before_retry = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(
        before_retry["type"], "presenter_admin_read",
        "{before_retry}"
    );
    assert!(
        before_retry["snapshot"]["values"]["persistence"]
            .as_str()
            .unwrap()
            .starts_with("Failed")
    );

    let mut retry = command("presenter_admin_write");
    retry["change"] = json!({"change":"settings_retry"});
    client.send(retry.clone()).await;
    let accepted = client
        .until(|r| r["command_id"] == retry["command_id"])
        .await;
    assert_eq!(accepted["type"], "presenter_admin_write", "{accepted}");
    assert_eq!(accepted["result"]["source"], "settings_presenter");
    assert_eq!(accepted["result"]["retry_requested"], true);
    bounded(SettingsApi::flush()).await.unwrap();

    client.send(inspect.clone()).await;
    let saved = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(saved["type"], "presenter_admin_read", "{saved}");
    assert!(
        saved["snapshot"]["values"]["persistence"]
            .as_str()
            .unwrap()
            .starts_with("Saved")
    );
    assert_eq!(
        saved["snapshot"]["values"]["persisted_revision"],
        saved["snapshot"]["revision"]
    );
    assert_eq!(repository.failures.load(Ordering::SeqCst), 1);
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_chat_diagnostics_are_character_scoped_bounded_and_content_free() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut empty = command("presenter_admin_read");
    empty["query"] = json!({"query":"chat_diagnostics","character":"Crazy","limit":1});
    client.send(empty.clone()).await;
    let no_requests = client
        .until(|r| r["command_id"] == empty["command_id"])
        .await;
    assert_eq!(no_requests["type"], "presenter_admin_read", "{no_requests}");
    assert_eq!(no_requests["snapshot"]["total_observed"], 0);
    assert_eq!(no_requests["snapshot"]["returned"], 0);
    let mut send = command("presenter_action");
    send["action"] = json!({"action":"chat_reply","character":"Crazy",
        "text":"PRIVATE_DIAGNOSTICS_INPUT_NEVER_LEAK"});
    client.send(send.clone()).await;
    let accepted = client.accepted(uuid(&send["command_id"])).await;
    let request_id = uuid(&accepted["request_id"]);
    fixture
        .call()
        .await
        .finish("PRIVATE_DIAGNOSTICS_ANSWER_NEVER_LEAK");
    assert_eq!(client.terminal(request_id).await["outcome"], "success");
    let mut inspect = command("presenter_admin_read");
    inspect["query"] = json!({"query":"chat_diagnostics","character":"Crazy","limit":1});
    client.send(inspect.clone()).await;
    let reply = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(reply["type"], "presenter_admin_read", "{reply}");
    assert_eq!(reply["snapshot"]["source"], "chat_presenter_snapshot");
    assert_eq!(reply["snapshot"]["returned"], 1);
    assert_eq!(
        reply["snapshot"]["requests"][0]["request_id"],
        json!(request_id)
    );
    assert_eq!(reply["snapshot"]["requests"][0]["outcome"], "success");
    assert_eq!(reply["snapshot"]["content_included"], false);
    let serial = reply.to_string();
    assert!(!serial.contains("PRIVATE_DIAGNOSTICS_INPUT_NEVER_LEAK"));
    assert!(!serial.contains("PRIVATE_DIAGNOSTICS_ANSWER_NEVER_LEAK"));
    assert!(!serial.contains("unused-fixture-key"));
    inspect["command_id"] = json!(Uuid::new_v4());
    inspect["query"]["character"] = json!("Kind");
    client.send(inspect.clone()).await;
    let isolated = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(isolated["type"], "presenter_admin_read", "{isolated}");
    assert_eq!(isolated["snapshot"]["returned"], 0);
    assert!(!isolated.to_string().contains(&request_id.to_string()));
    inspect["command_id"] = json!(Uuid::new_v4());
    inspect["query"]["character"] = json!("Crazy");
    inspect["query"]["limit"] = json!(101);
    client.send(inspect.clone()).await;
    let invalid = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(invalid["type"], "error", "{invalid}");
    fixture.shutdown().await;
}

#[tokio::test]
async fn prompt_catalog_refresh_uses_same_live_owner_as_gui() {
    let root = std::env::temp_dir().join(format!("nm-mcp-prompt-catalog-{}", Uuid::new_v4()));
    let crazy = root.join("Crazy");
    let first = crazy.join("Alpha");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::write(
        first.join("main_template.txt"),
        "PRIVATE_PROMPT_NEVER_EXPOSED",
    )
    .unwrap();
    let fixture = Fixture::with_prompt_catalog(root.clone()).await;
    let mut client = fixture.connect().await;
    let mut scan = command("presenter_admin_read");
    scan["query"] = json!({"query":"prompt_catalog","limit":1});
    client.send(scan.clone()).await;
    let initial = client
        .until(|r| r["command_id"] == scan["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_admin_read", "{initial}");
    assert_eq!(initial["snapshot"]["source"], "prompt_folder_presenter");
    assert_eq!(initial["snapshot"]["configured"], true);
    assert_eq!(initial["snapshot"]["total"], 1);
    assert_eq!(initial["snapshot"]["sets"][0]["name"], "Alpha");
    let second = crazy.join("Beta");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("main_template.txt"), "ANOTHER_PRIVATE_PROMPT").unwrap();
    scan["command_id"] = json!(Uuid::new_v4());
    client.send(scan.clone()).await;
    let updated = client
        .until(|r| r["command_id"] == scan["command_id"])
        .await;
    assert_eq!(updated["snapshot"]["total"], 2);
    assert_eq!(updated["snapshot"]["returned"], 1);
    assert_eq!(updated["snapshot"]["truncated"], true);
    assert!(!updated.to_string().contains("PRIVATE_PROMPT_NEVER_EXPOSED"));
    assert!(
        !updated
            .to_string()
            .contains(root.to_string_lossy().as_ref())
    );
    scan["command_id"] = json!(Uuid::new_v4());
    scan["query"]["limit"] = json!(20);
    client.send(scan.clone()).await;
    let all = client
        .until(|r| r["command_id"] == scan["command_id"])
        .await;
    assert_eq!(all["snapshot"]["returned"], 2);
    assert_eq!(all["snapshot"]["sets"][1]["name"], "Beta");
    std::fs::remove_dir_all(&first).unwrap();
    scan["command_id"] = json!(Uuid::new_v4());
    client.send(scan.clone()).await;
    let removed = client
        .until(|r| r["command_id"] == scan["command_id"])
        .await;
    assert_eq!(removed["snapshot"]["total"], 1);
    assert_eq!(removed["snapshot"]["sets"][0]["name"], "Beta");
    fixture.shutdown().await;
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn debug_select_projection_is_shared_and_private_by_default() {
    let fixture = Fixture::start(|_| {}).await;
    let expected = neuromita::presentation::project_character_debug(
        &fixture.app.presenter().characters,
        fixture.app.presenter().settings.as_ref(),
        fixture.app.presenter().prompts.as_deref(),
        CharacterId::Crazy,
    )
    .await
    .unwrap();
    let mut client = fixture.connect().await;
    let mut command = command("presenter_admin_read");
    command["query"] = json!({"query":"character_debug","character":"Crazy"});
    client.send(command.clone()).await;
    let sanitized = client
        .until(|r| r["command_id"] == command["command_id"])
        .await;
    assert_eq!(sanitized["type"], "presenter_admin_read", "{sanitized}");
    assert_eq!(sanitized["snapshot"]["source"], "character_debug_presenter");
    assert_eq!(sanitized["snapshot"]["field_count"], expected.len());
    assert_eq!(sanitized["snapshot"]["fields"], json!([]));
    assert_eq!(sanitized["snapshot"]["content_included"], false);
    assert!(!sanitized.to_string().contains("unused-fixture-key"));
    command["command_id"] = json!(Uuid::new_v4());
    command["query"]["include_content"] = json!(true);
    client.send(command.clone()).await;
    let denied = client
        .until(|r| r["command_id"] == command["command_id"])
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    assert!(!denied.to_string().contains("unused-fixture-key"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn conversation_refresh_reads_gui_snapshot_without_creating_another_owner() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut refresh = command("presenter_admin_read");
    refresh["query"] = json!({"query":"conversation_refresh","character":"Crazy"});
    let before_gui = fixture
        .app
        .presenter()
        .conversation
        .snapshot(CharacterId::Crazy)
        .await
        .unwrap();
    client.send(refresh.clone()).await;
    let before = client
        .until(|r| r["command_id"] == refresh["command_id"])
        .await;
    assert_eq!(before["type"], "presenter_admin_read", "{before}");
    assert_eq!(before["snapshot"]["source"], "conversation_presenter");
    assert_eq!(
        before["snapshot"]["visible_message_count"],
        before_gui.messages.len()
    );
    assert_eq!(
        before["snapshot"]["settings_revision"],
        before_gui.settings.revision
    );
    assert_eq!(before["snapshot"]["preset_id"], before_gui.preset.0);
    assert_eq!(
        before["snapshot"]["chat_revision"],
        before_gui.requests.revision
    );
    assert_eq!(before["snapshot"]["gui_window_rendered"], false);
    assert_eq!(before["snapshot"]["content_included"], false);
    let revision = before["snapshot"]["chat_revision"].as_u64().unwrap();

    let mut send = command("presenter_action");
    send["action"] = json!({"action":"chat_reply","character":"Crazy",
        "text":"PRIVATE_REFRESH_INPUT_NEVER_LEAK"});
    client.send(send.clone()).await;
    let accepted = client.accepted(uuid(&send["command_id"])).await;
    let request_id = uuid(&accepted["request_id"]);
    fixture
        .call()
        .await
        .finish("PRIVATE_REFRESH_OUTPUT_NEVER_LEAK");
    assert_eq!(client.terminal(request_id).await["outcome"], "success");
    refresh["command_id"] = json!(Uuid::new_v4());
    client.send(refresh.clone()).await;
    let after = client
        .until(|r| r["command_id"] == refresh["command_id"])
        .await;
    assert_eq!(after["type"], "presenter_admin_read", "{after}");
    assert_eq!(after["snapshot"]["refresh_completed"], true);
    assert!(after["snapshot"]["chat_revision"].as_u64().unwrap() > revision);
    assert_eq!(after["snapshot"]["character_request_count"], 1);
    let gui = fixture
        .app
        .presenter()
        .conversation
        .snapshot(CharacterId::Crazy)
        .await
        .unwrap();
    assert_eq!(
        after["snapshot"]["visible_message_count"],
        gui.messages.len()
    );
    assert_eq!(after["snapshot"]["chat_revision"], gui.requests.revision);
    assert_eq!(after["snapshot"]["version"]["through"], gui.at.through);
    assert_eq!(
        after["snapshot"]["version"]["generation"],
        gui.selection.generation
    );
    assert!(
        !after
            .to_string()
            .contains("PRIVATE_REFRESH_INPUT_NEVER_LEAK")
    );
    assert!(
        !after
            .to_string()
            .contains("PRIVATE_REFRESH_OUTPUT_NEVER_LEAK")
    );
    assert!(!after.to_string().contains("unused-fixture-key"));
    refresh["command_id"] = json!(Uuid::new_v4());
    refresh["query"]["character"] = json!("Kind");
    client.send(refresh.clone()).await;
    let isolated = client
        .until(|r| r["command_id"] == refresh["command_id"])
        .await;
    assert_eq!(isolated["type"], "presenter_admin_read", "{isolated}");
    assert_eq!(isolated["snapshot"]["character_request_count"], 0);
    assert!(!isolated.to_string().contains(&request_id.to_string()));
    fixture.shutdown().await;
}

#[tokio::test]
async fn chat_selection_presenter_is_single_owner_for_gui_and_local_control() {
    use neuromita::presentation::ChatSelection;

    let fixture = Fixture::start(|_| {}).await;
    let owner = fixture.app.presenter().selection.clone();
    let start = ChatSelection {
        character: CharacterId::Crazy,
        revision: 0,
    };
    assert_eq!(owner.snapshot(), start);
    let mut observer = owner.observe();
    let mut client = fixture.connect().await;
    let mut inspect = command("presenter_admin_read");
    inspect["query"] = json!({"query":"chat_selection"});
    client.send(inspect.clone()).await;
    let initial = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_admin_read", "{initial}");
    assert_eq!(initial["snapshot"]["selection"]["character"], "Crazy");
    assert_eq!(initial["snapshot"]["selection"]["revision"], 0);
    assert_eq!(initial["snapshot"]["gui_session_required"], false);

    let selected = bounded(owner.select(start, CharacterId::Kind))
        .await
        .unwrap();
    assert_eq!(selected.character, CharacterId::Kind);
    assert_eq!(selected.revision, 1);
    bounded(observer.changed()).await.unwrap();
    assert_eq!(*observer.borrow_and_update(), selected);
    assert_eq!(owner.snapshot(), selected);
    assert!(owner.select(start, CharacterId::Crazy).await.is_err());
    assert_eq!(owner.snapshot(), selected);
    assert_eq!(
        owner.select(selected, CharacterId::Kind).await.unwrap(),
        selected
    );
    assert_eq!(owner.snapshot().revision, 1);
    assert_eq!(
        owner
            .start_capture(start, Default::default(), Default::default())
            .await
            .unwrap_err(),
        "chat selection revision conflict"
    );

    inspect["command_id"] = json!(Uuid::new_v4());
    client.send(inspect.clone()).await;
    let after = client
        .until(|r| r["command_id"] == inspect["command_id"])
        .await;
    assert_eq!(after["snapshot"]["selection"]["character"], "Kind");
    assert_eq!(after["snapshot"]["selection"]["revision"], 1);
    assert!(!after.to_string().contains("session_id"));

    let mut unauthorized = command("presenter_admin_write");
    unauthorized["change"] = json!({
        "change":"chat_select","expected_character":"Kind", "expected_revision":1,
        "character":"Crazy", "authorization":"invalid-capability"
    });
    client.send(unauthorized.clone()).await;
    let denied = client
        .until(|r| r["command_id"] == unauthorized["command_id"])
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    assert_eq!(owner.snapshot(), selected);
    assert!(!denied.to_string().contains("invalid-capability"));

    let second = bounded(owner.select(selected, CharacterId::Crazy))
        .await
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(second.character, CharacterId::Crazy);
    bounded(observer.changed()).await.unwrap();
    assert_eq!(*observer.borrow_and_update(), second);
    let (left, right) = tokio::join!(
        owner.select(second, CharacterId::Kind),
        owner.select(second, CharacterId::Kind),
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "CAS must serialize concurrent selections"
    );
    assert_eq!(owner.snapshot().revision, 3);
    fixture.shutdown().await;
}

#[tokio::test]
async fn full_chat_trace_uses_presenter_and_denies_unprivileged_core_reads() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut reply = command("presenter_action");
    reply["action"] = json!({
        "action":"chat_reply", "character":"Crazy",
        "text":"PRIVATE_FULL_TRACE_INPUT"
    });
    client.send(reply.clone()).await;
    let accepted = client.accepted(uuid(&reply["command_id"])).await;
    let request_id = uuid(&accepted["request_id"]);
    fixture.call().await.finish("PRIVATE_FULL_TRACE_OUTPUT");
    assert_eq!(client.terminal(request_id).await["outcome"], "success");

    let published = fixture
        .app
        .presenter()
        .chat
        .latest_diagnostics(CharacterId::Crazy)
        .unwrap()
        .expect("latest retained GUI trace");
    assert_eq!(published.0.0, request_id);
    let full: Value = serde_json::from_str(&published.1).unwrap();
    assert!(
        full.is_object(),
        "GUI trace should be complete diagnostic JSON"
    );
    assert!(
        fixture
            .app
            .presenter()
            .chat
            .latest_diagnostics(CharacterId::Kind)
            .unwrap()
            .is_none(),
        "trace selection must be character-scoped"
    );

    let mut read = command("presenter_admin_read");
    read["query"] = json!({
        "query":"diagnostic_export","character":"Crazy",
        "authorization":"unauthorized-local-client"
    });
    client.send(read.clone()).await;
    let denied = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(denied["type"], "error", "{denied}");
    let frame = denied.to_string();
    assert!(!frame.contains("PRIVATE_FULL_TRACE_INPUT"));
    assert!(!frame.contains("PRIVATE_FULL_TRACE_OUTPUT"));
    assert!(!frame.contains("unauthorized-local-client"));
    assert!(!frame.contains("unused-fixture-key"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn presenter_speech_draft_is_single_reactive_owner_for_gui_and_mcp() {
    let fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut observer = fixture.app.presenter().speech_chat.observe();
    let mut read = command("presenter_admin_read");
    read["query"] = json!({"query":"speech_draft"});
    client.send(read.clone()).await;
    let initial = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(initial["type"], "presenter_admin_read", "{initial}");
    assert_eq!(initial["snapshot"]["draft_length"], 0);
    assert!(initial["snapshot"].get("draft").is_none());

    let mut write = command("presenter_admin_write");
    write["change"] = json!({"change":"speech_draft","expected_character":"Crazy",
        "expected_revision":0,"text":"PRIVATE_DRAFT_OWNED_BY_PRESENTER"});
    client.send(write.clone()).await;
    let saved = client
        .until(|r| r["command_id"] == write["command_id"])
        .await;
    assert_eq!(saved["type"], "presenter_admin_write", "{saved}");
    assert!(
        !saved
            .to_string()
            .contains("PRIVATE_DRAFT_OWNED_BY_PRESENTER")
    );
    bounded(observer.changed()).await.unwrap();
    let observed = observer.borrow_and_update().clone();
    assert_eq!(observed.draft, "PRIVATE_DRAFT_OWNED_BY_PRESENTER");
    assert_eq!(observed.selection.character, CharacterId::Crazy);
    assert_eq!(observed.selection.revision, 0);

    read["command_id"] = json!(Uuid::new_v4());
    client.send(read.clone()).await;
    let redacted = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(
        redacted["snapshot"]["draft_length"],
        "PRIVATE_DRAFT_OWNED_BY_PRESENTER".len()
    );
    assert!(redacted["snapshot"].get("draft").is_none());
    read["command_id"] = json!(Uuid::new_v4());
    read["query"]["include_content"] = json!(true);
    client.send(read.clone()).await;
    let included = client
        .until(|r| r["command_id"] == read["command_id"])
        .await;
    assert_eq!(
        included["snapshot"]["draft"],
        "PRIVATE_DRAFT_OWNED_BY_PRESENTER"
    );

    write["command_id"] = json!(Uuid::new_v4());
    write["change"]["expected_revision"] = json!(10);
    write["change"]["text"] = json!("stale");
    client.send(write.clone()).await;
    let stale = client
        .until(|r| r["command_id"] == write["command_id"])
        .await;
    assert_eq!(stale["type"], "error", "{stale}");
    assert_eq!(
        fixture.app.presenter().speech_chat.snapshot().draft,
        "PRIVATE_DRAFT_OWNED_BY_PRESENTER"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn conversation_attachment_fence_is_owned_by_presenter() {
    let fixture = Fixture::start(|_| {}).await;
    let conversation = &fixture.app.presenter().conversation;
    let snapshot = conversation.snapshot(CharacterId::Crazy).await.unwrap();
    let fence = conversation.current_fence().unwrap();
    assert_eq!(fence.at, snapshot.at);
    assert_eq!(fence.selection, snapshot.selection);
    fixture.shutdown().await;
}
