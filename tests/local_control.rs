use std::{future::Future, net::SocketAddr, num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterContext, CharacterId},
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{RequestHandle, RequestId, RequestOutcome, RequestSource, RequestState},
    server::{ServerConfig, ServerHandle, ServerSnapshot},
    settings::{SettingsApi, VoiceSettings, Volume},
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
    context: CharacterContext,
    reply: oneshot::Sender<Result<Arc<str>, GenerationError>>,
}

impl GenerationCall {
    fn finish(self, text: &str) {
        self.reply
            .send(Ok(Arc::from(text)))
            .expect("generation still running");
    }
}

struct ControlledGeneration(mpsc::UnboundedSender<GenerationCall>);

impl GenerationProvider for ControlledGeneration {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, context: CharacterContext, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(GenerationCall { context, reply })
                .map_err(|_| GenerationError::Transport("fixture stopped".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("fixture response dropped".into()))?
        })
    }
}

struct Fixture {
    app: ApplicationLifecycle,
    calls: mpsc::UnboundedReceiver<GenerationCall>,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    async fn start(configure: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::with_queue(8, configure).await
    }

    async fn with_queue(queue: usize, configure: impl FnOnce(&mut ServerConfig)) -> Self {
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
        let app = bounded(ApplicationLifecycle::start(config)).await.unwrap();
        bounded(SettingsApi::flush()).await.unwrap();
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

    async fn error(&mut self, message: &str) {
        let frame = self.until(|v| v["type"] == "error").await;
        assert!(
            frame["error"].as_str().unwrap().contains(message),
            "{frame}"
        );
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
    json!({"command_id": id, "command": "reply", "character": character, "text": text})
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
async fn rejected_commands_are_atomic_and_connection_recovers() {
    let mut fixture = Fixture::start(|_| {}).await;
    let mut client = fixture.connect().await;
    let mut invalid = vec![
        b"{\n".to_vec(),
        b"[]\n".to_vec(),
        b"null\n".to_vec(),
        b"\n".to_vec(),
        line(json!({"command":"settings"})),
        line(json!({"command_id":"invalid", "command":"test"})),
        line(command("unknown")),
    ];
    for name in ["test", "settings", "reply", "cancel", "set_volume"] {
        let mut value = command(name);
        value["extra"] = json!(true);
        match name {
            "reply" => {
                value["character"] = json!("mita");
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
        line(reply(Uuid::new_v4(), "mita", " \t")),
        line(json!({"command_id":Uuid::new_v4(), "command":"reply", "character":"mita", "text":4})),
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
    client
        .send(reply(id, "kind-mita", "valid after errors"))
        .await;
    let accepted = client.accepted(id).await;
    let call = fixture.call().await;
    assert_eq!(call.context.character_id, CharacterId::KindMita);
    assert_eq!(&*call.context.current_input, "valid after errors");
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
    let bytes = line(reply(id, "mita", "Привет"));
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
    rest.extend_from_slice(b"test\n");
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
    assert_eq!(&*call.context.current_input, "test");
    call.finish("second");
    client.terminal(uuid(&second["request_id"])).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn receipts_replay_terminal_results_and_evict_any_terminal_entry() {
    let mut fixture = Fixture::start(|server| server.receipt_capacity = positive(2)).await;
    let mut client = fixture.connect().await;
    let first = Uuid::new_v4();
    let first_command = reply(first, "mita", "long running");
    client.send(first_command.clone()).await;
    let original = client.accepted(first).await;
    let first_request = uuid(&original["request_id"]);
    let first_call = fixture.call().await;
    client.send(first_command.clone()).await;
    let replay = client.accepted(first).await;
    assert_eq!(replay["request_id"], original["request_id"]);
    assert_eq!(replay["replayed"], true);
    for changed in [
        reply(first, "mita", "changed"),
        reply(first, "kind-mita", "long running"),
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
    let second_command = reply(second, "kind-mita", "short running");
    client.send(second_command.clone()).await;
    let accepted = client.accepted(second).await;
    let second_request = uuid(&accepted["request_id"]);
    let second_call = fixture.call().await;
    client
        .send(reply(Uuid::new_v4(), "kind-mita", "full"))
        .await;
    client.error("too many tracked requests").await;
    assert!(fixture.calls.try_recv().is_err());
    second_call.finish("terminal result");
    let finished = client.terminal(second_request).await;
    assert_eq!(finished["outcome"], "success");
    client.send(reply(Uuid::new_v4(), "kind-mita", " ")).await;
    client.error("message must not be empty").await;
    client.send(second_command).await;
    let replay = client.accepted(second).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["request_id"], accepted["request_id"]);
    assert_eq!(client.terminal(second_request).await, finished);
    assert!(fixture.calls.try_recv().is_err());
    let third = Uuid::new_v4();
    client
        .send(reply(third, "kind-mita", "reuse terminal slot"))
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
    let original_command = reply(command_id, "mita", "survives disconnect");
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
        .send(reply(other_command, "kind-mita", "other client"))
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
            .state(CharacterId::Mita)
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
    client.send(reply(first, "mita", "running")).await;
    let first_request = uuid(&client.accepted(first).await["request_id"]);
    let first_call = fixture.call().await;
    let second = Uuid::new_v4();
    client.send(reply(second, "mita", "queued")).await;
    let second_request = uuid(&client.accepted(second).await["request_id"]);
    assert!(matches!(
        active(second_request).snapshot().state,
        RequestState::Pending
    ));
    let third = Uuid::new_v4();
    let third_command = reply(third, "mita", "retry after queue full");
    let timeline = fixture.app.timeline().len();
    client.send(third_command.clone()).await;
    let rejected = client.until(|v| v["type"] == "error").await;
    assert_eq!(rejected["command_id"], third.to_string());
    assert_eq!(rejected["error"], "request queue is full");
    assert!(fixture.calls.try_recv().is_err());
    assert_eq!(fixture.app.timeline().len(), timeline);
    first_call.finish("first done");
    client.terminal(first_request).await;
    let second_call = fixture.call().await;
    assert_eq!(&*second_call.context.current_input, "queued");
    client.send(third_command.clone()).await;
    let accepted = client.accepted(third).await;
    assert_eq!(accepted["replayed"], false);
    let third_request = uuid(&accepted["request_id"]);
    second_call.finish("second done");
    client.terminal(second_request).await;
    let third_call = fixture.call().await;
    assert_eq!(&*third_call.context.current_input, "retry after queue full");
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
            .state(CharacterId::Mita)
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
