use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    presentation::ChatPresenter,
    request::{
        CancelResult, ReactRequest, ReplyRequest, RequestHandle, RequestId, RequestOutcome,
        RequestSource, RequestState, SubmitError, UserInput,
    },
    timeline::EventContent,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

static CORE_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug)]
struct Call {
    path: String,
    body: Value,
    respond: oneshot::Sender<Reply>,
}

#[derive(Debug)]
enum Reply {
    Text(&'static str),
    Structured(&'static str),
    Status(u16),
}

struct GeminiServer {
    endpoint: String,
    calls: mpsc::UnboundedReceiver<Call>,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl GeminiServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1beta/", listener.local_addr().unwrap());
        let (send, calls) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let server_stop = stop.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    _ = server_stop.cancelled() => break,
                    connection = listener.accept() => {
                        let (stream, _) = connection.unwrap();
                        clients.spawn(serve(stream, send.clone()));
                    }
                    result = clients.join_next(), if !clients.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Self {
            endpoint,
            calls,
            stop,
            task,
        }
    }

    async fn next(&mut self) -> Call {
        timeout(Duration::from_secs(10), self.calls.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn shutdown(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}

async fn serve(mut stream: TcpStream, calls: mpsc::UnboundedSender<Call>) {
    let mut bytes = Vec::new();
    let boundary;
    loop {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).await.unwrap();
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            boundary = index + 4;
            break;
        }
        assert!(bytes.len() < 64 * 1024);
    }
    let header = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let path = header.lines().next().unwrap().to_owned();
    let size: usize = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    assert!(!header.to_ascii_lowercase().contains("x-goog-api-key:"));
    assert!(!header.to_ascii_lowercase().contains("authorization:"));
    assert!(path.contains(":generateContent?key=test-key "));
    while bytes.len() < boundary + size {
        let mut chunk = [0; 4096];
        let count = stream.read(&mut chunk).await.unwrap();
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let body = serde_json::from_slice(&bytes[boundary..boundary + size]).unwrap();
    let (respond, response) = oneshot::channel();
    calls
        .send(Call {
            path,
            body,
            respond,
        })
        .unwrap();
    let Ok(response) = response.await else {
        return;
    };
    let (status, body) = match response {
        Reply::Text(text) => (200, json!({"candidates":[{"content":{"role":"model","parts":[{"text":"private reasoning","thought":true},{"text":json!({"segments":[{"text":text}]}).to_string()}]},"finishReason":"STOP"}]}).to_string()),
        Reply::Structured(text) => (200, json!({"candidates":[{"content":{"role":"model","parts":[{"text":text}]},"finishReason":"STOP"}]}).to_string()),
        Reply::Status(status) => (status, "{}".to_owned()),
    };
    let response = format!(
        "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn preset(endpoint: &str) -> GenerationPreset {
    let settings = GenerationSettings::new(0.7, 512, Duration::from_secs(10)).unwrap();
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::new(endpoint, "test-model", "test-key", settings).unwrap(),
    ))
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(2).unwrap(),
        discovery_buffer: NonZeroUsize::new(32).unwrap(),
        character_commands: NonZeroUsize::new(16).unwrap(),
        presenter_updates: NonZeroUsize::new(32).unwrap(),
        retained_finished: NonZeroUsize::new(20).unwrap(),
    }
}

struct CheckedProvider(GeminiProvider);

impl GenerationProvider for CheckedProvider {
    fn kind(&self) -> neuromita::generation::ProviderKind {
        neuromita::generation::ProviderKind::Gemini
    }

    fn generate(
        &self,
        context: neuromita::character::CharacterContext,
        template: ProviderTemplate,
    ) -> neuromita::generation::ProviderFuture<'_> {
        Box::pin(async move {
            assert_ne!(
                context
                    .view
                    .messages()
                    .last()
                    .map(|message| message.content.as_str())
                    .unwrap_or(""),
                "panic provider",
                "injected provider panic"
            );
            self.0.generate(context, template).await
        })
    }
}

async fn finished(handle: &RequestHandle) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(10), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

async fn shown(presenter: &ChatPresenter, id: RequestId, terminal: bool) {
    timeout(Duration::from_secs(10), async {
        let mut model = presenter.observe();
        loop {
            let present = {
                model.borrow_and_update().requests.iter().any(|row| {
                    row.metadata.id == id && (!terminal || row.snapshot.state.is_terminal())
                })
            };
            if present {
                return;
            }
            model.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

fn user_texts(body: &Value) -> Vec<&str> {
    body["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["role"] == "user")
        .map(|item| item["parts"][0]["text"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presenter_to_provider_observation_cancellation_and_commit() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    assert!(matches!(
        ChatApi::observe_requests(),
        Err(SubmitError::Uninitialized | SubmitError::Stopped)
    ));
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let config = ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let presenter = &app.presenter().chat;
    assert!(presenter.reply(CharacterId::Mita, " ".into()).is_err());

    let first = presenter.reply(CharacterId::Mita, "first".into()).unwrap();
    let first_call = server.next().await;
    assert_eq!(
        first_call.path,
        "POST /v1beta/models/test-model:generateContent?key=test-key HTTP/1.1"
    );
    assert_eq!(user_texts(&first_call.body), ["first"]);
    assert_eq!(first_call.body["generationConfig"]["candidateCount"], 1);
    assert_eq!(
        first_call.body["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        first_call.body["generationConfig"]["responseSchema"]["required"],
        json!(["segments"])
    );
    assert!(
        first_call.body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Мита")
    );

    let queued = presenter
        .reply(CharacterId::Mita, "never starts".into())
        .unwrap();
    let running_cancel = presenter
        .reply(CharacterId::Mita, "cancel during generation".into())
        .unwrap();
    assert!(matches!(
        presenter.reply(CharacterId::Mita, "queue overflow".into()),
        Err(neuromita::presentation::PresenterError::Submit(
            SubmitError::QueueFull
        ))
    ));
    assert_eq!(queued.cancel(), CancelResult::Cancelled);
    assert!(matches!(
        finished(&queued).await.state,
        RequestState::Cancelled
    ));
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );

    let external = ChatApi::reply(ReplyRequest::new(
        CharacterId::KindMita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::KindMita,
            UserInput::new("external client".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Client(Uuid::new_v4()),
    ))
    .unwrap();
    let external_call = server.next().await;
    assert_eq!(user_texts(&external_call.body), ["external client"]);
    shown(presenter, external.id(), false).await;
    external_call
        .respond
        .send(Reply::Text("kind reply"))
        .unwrap();
    assert!(matches!(
        finished(&external).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    shown(presenter, external.id(), true).await;
    assert!(matches!(
        first.snapshot().state,
        RequestState::Processing(_)
    ));

    first_call.respond.send(Reply::Text("first reply")).unwrap();
    let first_result = finished(&first).await;
    match &first_result.state {
        RequestState::Finished(RequestOutcome::Success(response)) => {
            assert_eq!(&*response.text, "first reply")
        }
        state => panic!("unexpected state: {state:?}"),
    }
    assert!(matches!(first.cancel(), CancelResult::AlreadyFinished));
    let mut late_observer = first.observe();
    assert!(
        late_observer
            .next()
            .await
            .unwrap()
            .unwrap()
            .state
            .is_terminal()
    );
    assert!(late_observer.next().await.unwrap().is_none());

    let cancelled_call = server.next().await;
    let cancelled_user = user_texts(&cancelled_call.body);
    assert_eq!(cancelled_user.len(), 4);
    assert_eq!(cancelled_user[0], "first");
    assert_eq!(cancelled_user[1], "never starts");
    assert!(cancelled_user[2].contains("cancel during generation"));
    assert!(cancelled_user[2].contains("на которое готовится текущий ответ"));
    assert_eq!(cancelled_user[3], "queue overflow");
    assert_eq!(cancelled_call.body["contents"][4]["role"], "model");
    assert_eq!(
        cancelled_call.body["contents"][4]["parts"][0]["text"],
        "first reply"
    );
    assert_eq!(running_cancel.cancel(), CancelResult::Requested);
    assert!(matches!(
        finished(&running_cancel).await.state,
        RequestState::Cancelled
    ));
    let _ = cancelled_call
        .respond
        .send(Reply::Text("must not be accepted"));
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        1
    );

    let failed = presenter
        .reply(CharacterId::Mita, "HTTP failure".into())
        .unwrap();
    server
        .next()
        .await
        .respond
        .send(Reply::Status(503))
        .unwrap();
    assert!(matches!(
        finished(&failed).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));

    let panicked = presenter
        .reply(CharacterId::Mita, "panic provider".into())
        .unwrap();
    assert!(matches!(
        finished(&panicked).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));

    let next = presenter
        .reply(CharacterId::Mita, "after failure".into())
        .unwrap();
    let next_call = server.next().await;
    assert!(!next_call.body.to_string().contains("must not be accepted"));
    assert!(next_call.body.to_string().contains("never starts"));
    assert!(next_call.body.to_string().contains("queue overflow"));
    next_call.respond.send(Reply::Text("second reply")).unwrap();
    assert!(matches!(
        finished(&next).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    shown(presenter, next.id(), true).await;
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        2
    );
    let events = app.timeline();
    let replies: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.content, EventContent::CharacterReply { .. }))
        .collect();
    assert_eq!(replies.len(), 3);
    assert!(
        replies
            .iter()
            .any(|event| event.request_id == Some(first.id()))
    );
    assert!(events.iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "never starts")
    }));
    assert!(!events.iter().any(|event| {
        event.request_id == Some(queued.id())
            && matches!(event.content, EventContent::CharacterReply { .. })
    }));

    let interrupted = presenter
        .reply(CharacterId::Mita, "stop application".into())
        .unwrap();
    let stopped_call = server.next().await;
    let pending = presenter
        .reply(CharacterId::Mita, "stop pending".into())
        .unwrap();
    app.shutdown().await.unwrap();
    assert!(matches!(
        finished(&interrupted).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&pending).await.state,
        RequestState::Cancelled
    ));
    drop(stopped_call);
    assert!(matches!(
        ChatApi::observe_requests(),
        Err(SubmitError::Stopped)
    ));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsupported_structured_effect_fails_without_committing_or_synthesizing() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset(&server.endpoint),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let handle = app
        .presenter()
        .chat
        .reply(CharacterId::Mita, "act now".into())
        .unwrap();
    let call = server.next().await;
    call.respond
        .send(Reply::Structured(
            r#"{"segments":[{"text":"I will move","commands":["move"]}]}"#,
        ))
        .unwrap();

    assert!(matches!(
        finished(&handle).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    assert!(
        !app.timeline()
            .iter()
            .any(|event| { matches!(event.content, EventContent::CharacterReply { .. }) })
    );
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_generation_pinned_to_a_reselected_branch_cannot_commit() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let pending = app
        .presenter()
        .chat
        .reply(CharacterId::Mita, "pinned".into())
        .unwrap();
    let active = server.next().await;
    let selected = app.timeline_selection();
    let changed = app
        .select_timeline_branch(selected.branch_id, selected)
        .unwrap();
    assert!(changed.generation > selected.generation);
    assert!(matches!(
        finished(&pending).await.state,
        RequestState::Cancelled
    ));
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    assert!(
        !app.timeline()
            .iter()
            .any(|event| { matches!(event.content, EventContent::CharacterReply { .. }) })
    );
    let fresh = app
        .presenter()
        .chat
        .reply(CharacterId::Mita, "new selection".into())
        .unwrap();
    let next = server.next().await;
    drop(active);
    next.respond.send(Reply::Text("current result")).unwrap();
    assert!(matches!(
        finished(&fresh).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_reaction_uses_newer_known_player_message_at_dispatch() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let first = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("first message".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let first_call = server.next().await;
    let cause = app
        .character(CharacterId::Mita)
        .unwrap()
        .observe(UserInput::new("door opened".into()).unwrap())
        .await
        .unwrap();
    let reaction = ChatApi::react(ReactRequest::new(
        CharacterId::Mita,
        cause,
        Arc::clone(UserInput::new("React to the door".into()).unwrap().text()),
        RequestSource::Presenter,
    ))
    .unwrap();
    let question = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("Why did you open the door?".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    assert!(app.timeline().iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "Why did you open the door?")
    }));

    first_call
        .respond
        .send(Reply::Text("first answer"))
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let reaction_call = server.next().await;
    let user = user_texts(&reaction_call.body);
    assert!(user.contains(&"Why did you open the door?"));
    assert!(user.iter().any(|text| text.contains("React to the door")));
    assert!(
        user.iter()
            .any(|text| text.contains("отреагировать на отмеченное событие"))
    );
    assert_eq!(
        user.iter()
            .filter(|text| **text == "Why did you open the door?")
            .count(),
        1
    );
    reaction_call
        .respond
        .send(Reply::Text("because it was closed"))
        .unwrap();
    assert!(matches!(
        finished(&reaction).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let question_call = server.next().await;
    assert_eq!(
        user_texts(&question_call.body)
            .iter()
            .filter(|text| text.contains("Why did you open the door?"))
            .count(),
        1
    );
    question_call
        .respond
        .send(Reply::Text("the door is open"))
        .unwrap();
    assert!(matches!(
        finished(&question).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_input_survives_selection_change_but_stale_generation_does_not_start() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let first = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("active request".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let blocked = server.next().await;
    let waiting = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("queued and already spoken".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let selected = app.timeline_selection();
    app.select_timeline_branch(selected.branch_id, selected)
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&waiting).await.state,
        RequestState::Cancelled
    ));
    assert!(server.calls.try_recv().is_err());
    let events = app.timeline();
    assert!(events.iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "queued and already spoken")
    }));
    assert!(!events.iter().any(|event| {
        event.request_id == Some(waiting.id())
            && matches!(event.content, EventContent::CharacterReply { .. })
    }));
    drop(blocked);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_player_message_is_visible_without_scheduling_generation() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(CheckedProvider(GeminiProvider::new().unwrap()));
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let input = UserInput::new("Message without an LLM request".into()).unwrap();
    let event_id = input.event_id();
    assert_eq!(
        ChatApi::publish_message(CharacterId::Mita, input).unwrap(),
        event_id
    );
    ChatApi::publish_message(
        CharacterId::KindMita,
        UserInput::new("Only KindMita can hear this".into()).unwrap(),
    )
    .unwrap();
    let world = app.timeline();
    assert_eq!(world.len(), 2);
    assert!(
        world
            .iter()
            .any(|event| event.id == event_id && event.request_id.is_none())
    );

    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("New question".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = server.next().await;
    assert_eq!(
        user_texts(&call.body),
        ["Message without an LLM request", "New question"]
    );
    assert!(
        !call
            .body
            .to_string()
            .contains("Only KindMita can hear this")
    );
    call.respond.send(Reply::Text("response")).unwrap();
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn character_reset_cancels_only_its_running_generation_and_keeps_input_fact() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let mita = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("Question before reset".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let mita_call = server.next().await;
    let kind = ChatApi::reply(ReplyRequest::new(
        CharacterId::KindMita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::KindMita,
            UserInput::new("KindMita stays active".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let kind_call = server.next().await;

    let selected = app.timeline_selection();
    let reset_id = neuromita::timeline::EventId::new();
    assert_eq!(
        app.reset_character_context(selected, CharacterId::Mita, reset_id),
        Ok(reset_id)
    );
    assert!(matches!(
        finished(&mita).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(kind.snapshot().state, RequestState::Processing(_)));
    assert_eq!(app.at().through, 3);
    assert_eq!(
        app.reset_character_context(selected, CharacterId::Mita, reset_id),
        Ok(reset_id)
    );
    assert_eq!(app.at().through, 3);

    let mita_view = app
        .character(CharacterId::Mita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert!(mita_view.view.messages().is_empty());
    assert_eq!(mita_view.state.accepted_replies, 0);

    kind_call
        .respond
        .send(Reply::Text("KindMita answered"))
        .unwrap();
    assert!(matches!(
        finished(&kind).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let old = app.timeline();
    assert!(old.iter().any(|event| {
        matches!(&event.content, EventContent::PlayerMessage { text } if text.as_ref() == "Question before reset")
    }));
    assert!(!old.iter().any(|event| {
        event.request_id == Some(mita.id())
            && matches!(event.content, EventContent::CharacterReply { .. })
    }));

    let next = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("After reset".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let next_call = server.next().await;
    assert_eq!(user_texts(&next_call.body), ["After reset"]);
    next_call.respond.send(Reply::Text("New answer")).unwrap();
    assert!(matches!(
        finished(&next).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    drop(mita_call);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_reset_is_rejected_without_cancelling_current_request() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let selected = app.timeline_selection();
    app.select_timeline_branch(selected.branch_id, selected)
        .unwrap();
    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("Valid after reselection".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = server.next().await;
    let at = app.at();
    assert!(
        app.reset_character_context(
            selected,
            CharacterId::Mita,
            neuromita::timeline::EventId::new()
        )
        .is_err()
    );
    assert_eq!(app.at(), at);
    assert!(matches!(
        reply.snapshot().state,
        RequestState::Processing(_)
    ));
    call.respond.send(Reply::Text("answer")).unwrap();
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

struct PausedVoice(mpsc::UnboundedSender<oneshot::Sender<()>>);

impl neuromita::voice::VoiceProvider for PausedVoice {
    fn synthesize(
        &self,
        _text: Arc<str>,
        _options: neuromita::voice::VoiceOptions,
    ) -> neuromita::voice::VoiceFuture<'_> {
        Box::pin(async {
            let (send, receive) = oneshot::channel();
            self.0.send(send).unwrap();
            let _ = receive.await;
            Ok(neuromita::voice::AudioArtifact {
                format: neuromita::voice::AudioFormat::Wave,
                bytes: Arc::from([0u8; 4]),
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_during_voice_generation_cancels_before_response_is_accepted() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let (voice_started, mut voice_calls) = mpsc::unbounded_channel();
    let mut config = ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.voice = Arc::new(PausedVoice(voice_started));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    app.presenter()
        .settings
        .voice
        .in_launcher
        .set(true)
        .unwrap();
    app.presenter()
        .settings
        .voice
        .voice
        .set("voice-id".into())
        .unwrap();
    app.presenter()
        .settings
        .voice
        .volume
        .set(neuromita::settings::Volume::new(80).unwrap())
        .unwrap();
    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("Question requiring voice".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let provider_call = server.next().await;
    provider_call
        .respond
        .send(Reply::Text("spoken reply"))
        .unwrap();
    let voice_release = timeout(Duration::from_secs(10), voice_calls.recv())
        .await
        .unwrap()
        .unwrap();
    let reset_id = neuromita::timeline::EventId::new();
    assert_eq!(
        app.reset_character_context(app.timeline_selection(), CharacterId::Mita, reset_id),
        Ok(reset_id)
    );
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Cancelled
    ));
    assert!(voice_release.send(()).is_err());
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_cancels_queued_reaction_admitted_before_reset() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let cause = app
        .character(CharacterId::Mita)
        .unwrap()
        .observe(UserInput::new("Known door opened".into()).unwrap())
        .await
        .unwrap();
    let first = ChatApi::reply(ReplyRequest::new(
        CharacterId::Mita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::Mita,
            UserInput::new("First blocking request".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let blocked = server.next().await;
    let reaction = ChatApi::react(ReactRequest::new(
        CharacterId::Mita,
        cause,
        Arc::clone(UserInput::new("React to door".into()).unwrap().text()),
        RequestSource::Presenter,
    ))
    .unwrap();
    let reset = neuromita::timeline::EventId::new();
    app.reset_character_context(app.timeline_selection(), CharacterId::Mita, reset)
        .unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Cancelled
    ));
    assert!(matches!(
        finished(&reaction).await.state,
        RequestState::Cancelled
    ));
    assert!(server.calls.try_recv().is_err());
    assert_eq!(app.at().through, 3);
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    drop(blocked);
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn historical_reply_cannot_restore_knowledge_across_a_character_reset() {
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    ChatApi::publish_message(
        CharacterId::Mita,
        UserInput::new("Private knowledge before reset".into()).unwrap(),
    )
    .unwrap();
    let old = app.at();
    app.reset_character_context(
        app.timeline_selection(),
        CharacterId::Mita,
        neuromita::timeline::EventId::new(),
    )
    .unwrap();

    let stale = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::Mita,
            neuromita::api::ChatApi::publish_message(
                CharacterId::Mita,
                UserInput::new("Try old context".into()).unwrap(),
            )
            .unwrap(),
            RequestSource::Presenter,
        )
        .at(neuromita::timeline::ContextPosition::At(old)),
    )
    .unwrap();
    assert!(matches!(
        finished(&stale).await.state,
        RequestState::Cancelled
    ));
    assert!(server.calls.try_recv().is_err());
    assert_eq!(
        app.character(CharacterId::Mita)
            .unwrap()
            .get_context(old)
            .await
            .unwrap()
            .view
            .messages()[0]
            .content,
        "Private knowledge before reset"
    );

    let independent = ChatApi::reply(ReplyRequest::new(
        CharacterId::KindMita,
        neuromita::api::ChatApi::publish_message(
            CharacterId::KindMita,
            UserInput::new("Your own earlier view".into()).unwrap(),
        )
        .unwrap(),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = server.next().await;
    assert_eq!(user_texts(&call.body), ["Your own earlier view"]);
    assert!(
        !call
            .body
            .to_string()
            .contains("Private knowledge before reset")
    );
    call.respond
        .send(Reply::Text("independent answer"))
        .unwrap();
    assert!(matches!(
        finished(&independent).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_character_messages_use_shared_conversation_audience() {
    use neuromita::timeline::conversation::{
        ConversationAudience, ConversationMessage, ConversationSpeaker,
    };
    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let preset = preset(&server.endpoint);
    let generator = ResponseGenerator::new([
        Arc::new(GeminiProvider::new().unwrap()) as Arc<dyn GenerationProvider>
    ])
    .unwrap();
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset.clone(),
        generator,
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let mut audience = ConversationAudience::private_to(CharacterId::Mita);
    audience.seen_by.insert(CharacterId::KindMita);
    audience.targets.extend([
        ConversationSpeaker::Player,
        ConversationSpeaker::Character(CharacterId::KindMita),
    ]);
    audience.participants.extend([
        ConversationSpeaker::Player,
        ConversationSpeaker::Character(CharacterId::Mita),
        ConversationSpeaker::Character(CharacterId::KindMita),
    ]);

    let mut shared_input = ConversationAudience::player_to(CharacterId::Mita);
    shared_input.seen_by.insert(CharacterId::KindMita);
    shared_input
        .participants
        .insert(ConversationSpeaker::Character(CharacterId::KindMita));

    let mut incoming = ConversationMessage::new(
        "shared-player-input",
        ConversationSpeaker::Player,
        json!("Say hello to Kind Mita"),
        [],
    );
    incoming.audience = shared_input;
    let message_id = incoming.id;
    app.publish_conversation_message(app.timeline_selection(), incoming)
        .unwrap();
    let reply = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::Mita,
            message_id,
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(audience.clone()),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Text("Hello, Kind Mita!")).unwrap();
    assert!(matches!(
        finished(&reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let mita = app
        .character(CharacterId::Mita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let kind = app
        .character(CharacterId::KindMita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let source = mita.view.provider_messages();
    let listener = kind.view.provider_messages();
    assert_eq!(source.len(), 2);
    assert_eq!(listener.len(), 2);
    assert_eq!(listener[0]["speaker"], "player");
    assert_eq!(listener[0]["targets"], json!(["mita"]));
    assert_eq!(
        listener[0]["timeline_event_id"],
        source[0]["timeline_event_id"]
    );
    assert_eq!(source[1]["role"], "assistant");
    assert_eq!(listener[1]["role"], "user");
    assert_eq!(listener[1]["speaker"], "mita");
    assert_eq!(
        source[1]["timeline_event_id"],
        listener[1]["timeline_event_id"]
    );
    assert_eq!(source[1]["targets"], json!(["player", "kind-mita"]));
    assert_eq!(
        listener[1]["participants"],
        json!(["player", "mita", "kind-mita"])
    );
    assert_eq!(listener[1]["content"], "Hello, Kind Mita!");

    let mut private = ConversationAudience::private_to(CharacterId::KindMita);
    private
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Mita));
    private.participants.extend([
        ConversationSpeaker::Character(CharacterId::KindMita),
        ConversationSpeaker::Character(CharacterId::Mita),
    ]);
    let private_reply = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::KindMita,
            neuromita::api::ChatApi::publish_message(
                CharacterId::KindMita,
                UserInput::new("A private reply".into()).unwrap(),
            )
            .unwrap(),
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(private.clone()),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Text("Secret response")).unwrap();
    assert!(matches!(
        finished(&private_reply).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let mita_now = app
        .character(CharacterId::Mita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let kind_now = app
        .character(CharacterId::KindMita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(mita_now.view.count(), 2);
    assert_eq!(kind_now.view.count(), 4);
    assert_eq!(
        kind_now.view.provider_messages()[3]["targets"],
        json!(["mita"])
    );
    assert_eq!(kind_now.view.provider_messages()[3]["role"], "assistant");

    let react = ChatApi::react(
        ReactRequest::new(
            CharacterId::KindMita,
            neuromita::timeline::EventId(
                Uuid::parse_str(source[1]["timeline_event_id"].as_str().unwrap()).unwrap(),
            ),
            Arc::clone(UserInput::new("React to Mita".into()).unwrap().text()),
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(audience.clone()),
    )
    .unwrap();
    let call = server.next().await;
    call.respond.send(Reply::Text("I heard you!")).unwrap();
    assert!(matches!(
        finished(&react).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let now_mita = app
        .character(CharacterId::Mita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let now_kind = app
        .character(CharacterId::KindMita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(now_mita.view.count(), 3);
    assert_eq!(now_kind.view.count(), 5);
    assert_eq!(now_mita.view.provider_messages()[2]["speaker"], "kind-mita");
    assert_eq!(now_kind.view.provider_messages()[4]["role"], "assistant");
    let initiate = ChatApi::initiate(
        neuromita::request::InitiateRequest::new(
            CharacterId::Mita,
            Arc::clone(
                UserInput::new("Say something to both".into())
                    .unwrap()
                    .text(),
            ),
            RequestSource::Client(Uuid::new_v4()),
        )
        .with_audience(audience),
    )
    .unwrap();
    let call = server.next().await;
    call.respond
        .send(Reply::Text("Everyone can hear me"))
        .unwrap();
    assert!(matches!(
        finished(&initiate).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let mita_final = app
        .character(CharacterId::Mita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let kind_final = app
        .character(CharacterId::KindMita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(mita_final.view.count(), 4);
    assert_eq!(kind_final.view.count(), 6);
    assert_eq!(mita_final.view.provider_messages()[3]["role"], "assistant");
    assert_eq!(kind_final.view.provider_messages()[5]["role"], "user");

    let missing_speaker = ConversationAudience::private_to(CharacterId::KindMita);
    let previous_head = app.at().through;
    let mut message = ConversationMessage::new(
        "player-unseen-by-mita",
        ConversationSpeaker::Player,
        json!("Mita cannot hear this"),
        [],
    );
    message.audience = missing_speaker;
    let message_id = message.id;
    app.publish_conversation_message(app.timeline_selection(), message)
        .unwrap();
    assert!(matches!(
        ChatApi::reply(ReplyRequest::new(
            CharacterId::Mita,
            message_id,
            RequestSource::Client(Uuid::new_v4())
        )),
        Err(SubmitError::InvalidTargetEvent)
    ));
    assert_eq!(app.at().through, previous_head + 1);

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn preset_is_resolved_only_when_queued_generation_begins() {
    use std::collections::BTreeMap;

    use neuromita::{
        generation::PresetCatalog,
        settings::{CharactersSettings, PresetId, PresetSelection, PresetSettings, SettingsApi},
    };

    let _serial = CORE_TEST_MUTEX.lock().await;
    let mut server = GeminiServer::start().await;
    let named = |name: &str| {
        GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::new(
                &server.endpoint,
                name,
                "test-key",
                GenerationSettings::new(0.7, 512, Duration::from_secs(10)).unwrap(),
            )
            .unwrap(),
        ))
    };
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let mut config = ApplicationConfig::minimal(
        named("alpha"),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    );
    config.presets = PresetCatalog::new(BTreeMap::from([
        (PresetId(0), named("alpha")),
        (PresetId(1), named("beta")),
    ]));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let first = app
        .presenter()
        .chat
        .reply(CharacterId::Mita, "one".into())
        .unwrap();
    let first_call = server.next().await;
    assert!(first_call.path.contains("models/alpha:generateContent"));
    let waiting = app
        .presenter()
        .chat
        .reply(CharacterId::Mita, "two".into())
        .unwrap();

    SettingsApi::edit::<PresetSettings>(|settings| {
        settings.active = PresetId(1);
    })
    .unwrap();
    SettingsApi::edit::<CharactersSettings>(|settings| {
        settings.mita.presets.react = PresetSelection::Specific(PresetId(0));
    })
    .unwrap();

    first_call.respond.send(Reply::Text("first reply")).unwrap();
    assert!(matches!(
        finished(&first).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let second_call = server.next().await;
    assert!(second_call.path.contains("models/beta:generateContent"));
    second_call
        .respond
        .send(Reply::Text("second reply"))
        .unwrap();
    assert!(matches!(
        finished(&waiting).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let event = ChatApi::publish_message(
        CharacterId::Mita,
        UserInput::new("react to this".into()).unwrap(),
    )
    .unwrap();
    let reaction = ChatApi::react(ReactRequest::new(
        CharacterId::Mita,
        event,
        "react",
        RequestSource::Presenter,
    ))
    .unwrap();
    let reaction_call = server.next().await;
    assert!(reaction_call.path.contains("models/alpha:generateContent"));
    reaction_call.respond.send(Reply::Text("reaction")).unwrap();
    assert!(matches!(
        finished(&reaction).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    SettingsApi::edit::<CharactersSettings>(|settings| {
        settings.mita.presets.reply = PresetSelection::Specific(PresetId(999));
    })
    .unwrap();
    let missing = app
        .presenter()
        .chat
        .reply(CharacterId::Mita, "missing".into())
        .unwrap();
    assert!(matches!(
        finished(&missing).await.state,
        RequestState::Finished(RequestOutcome::InternalError(_))
    ));
    assert!(server.calls.try_recv().is_err());

    app.shutdown().await.unwrap();
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preset_api_exposes_separate_catalog_crud_without_touching_settings() {
    use neuromita::{
        api::PresetApi,
        generation::{GeminiPresetDefinition, PresetCatalogError, PresetDefinition, PresetRecord},
        settings::SettingsApi,
    };

    let _serial = CORE_TEST_MUTEX.lock().await;
    assert!(matches!(
        PresetApi::list(),
        Err(PresetCatalogError::Unavailable)
    ));
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        preset("http://localhost:8800/v1beta/"),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();

    let settings_before = SettingsApi::snapshot_all().unwrap();
    let make = |model: &str| PresetRecord {
        name: model.into(),
        definition: PresetDefinition::Gemini(GeminiPresetDefinition {
            endpoint: "http://127.0.0.1:8800/v1beta/".into(),
            model: model.into(),
            credential_env: "EXAMPLE_GEMINI_KEY".into(),
            temperature: 0.7,
            max_output_tokens: 128,
            timeout_ms: 2000,
        }),
    };
    let id = PresetApi::create(make("first")).unwrap();
    assert_eq!(PresetApi::list().unwrap().len(), 1);
    PresetApi::update(id, make("second")).unwrap();
    assert_eq!(PresetApi::list().unwrap()[0].1.name, "second");
    assert_eq!(
        SettingsApi::snapshot_all().unwrap().revision,
        settings_before.revision
    );
    PresetApi::delete(id).unwrap();
    assert!(PresetApi::list().unwrap().is_empty());
    app.shutdown().await.unwrap();
    assert!(matches!(
        PresetApi::list(),
        Err(PresetCatalogError::Unavailable)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_bootstrap_and_preset_presenter_survive_restart() {
    use neuromita::{
        generation::{GeminiPresetDefinition, PresetDefinition, PresetRecord},
        settings::{PresetSelection, SettingsApi},
    };

    let _serial = CORE_TEST_MUTEX.lock().await;
    let directory = std::env::temp_dir().join(format!("neuromita-catalog-{}", Uuid::new_v4()));
    let record = |model: &str| PresetRecord {
        name: model.to_owned(),
        definition: PresetDefinition::Gemini(GeminiPresetDefinition {
            endpoint: "http://127.0.0.1:8100/v1beta/".to_owned(),
            model: model.to_owned(),
            credential_env: "EXAMPLE_GEMINI_KEY".to_owned(),
            temperature: 0.7,
            max_output_tokens: 256,
            timeout_ms: 5000,
        }),
    };
    let setup = || {
        let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
        ApplicationConfig::persistent(
            preset("http://127.0.0.1:8100/v1beta/"),
            ResponseGenerator::new([provider]).unwrap(),
            limits(),
            "127.0.0.1:0".parse().unwrap(),
            &directory,
        )
        .unwrap()
    };

    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    let presets = &app.presenter().presets;
    let mut changes = presets.observe();
    assert_eq!(changes.borrow_and_update().revision, 0);
    let initial_settings_revision = SettingsApi::snapshot_all().unwrap().revision;
    let id = presets.create(record("first-model")).unwrap();
    changes.changed().await.unwrap();
    assert_eq!(changes.borrow_and_update().entries[&id].name, "first-model");
    assert_eq!(
        SettingsApi::snapshot_all().unwrap().revision,
        initial_settings_revision
    );
    presets.update(id, record("second-model")).unwrap();
    changes.changed().await.unwrap();
    assert_eq!(
        changes.borrow_and_update().entries[&id].name,
        "second-model"
    );
    app.presenter()
        .settings
        .characters
        .mita
        .reply
        .set(PresetSelection::Specific(id))
        .unwrap();
    app.presenter().settings.presets.active.set(id).unwrap();
    app.shutdown().await.unwrap();

    assert!(directory.join("settings.json").exists());
    let stored = std::fs::read_to_string(directory.join("presets.json")).unwrap();
    assert!(stored.contains("second-model"));
    assert!(stored.contains("EXAMPLE_GEMINI_KEY"));
    assert!(!stored.contains("test-key"));

    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    assert_eq!(app.presenter().presets.list()[0].0, id);
    assert_eq!(app.presenter().presets.list()[0].1.name, "second-model");
    assert_eq!(app.presenter().settings.presets.active.get(), id);
    assert_eq!(
        app.presenter().settings.characters.mita.reply.get(),
        PresetSelection::Specific(id)
    );
    app.presenter().presets.delete(id).unwrap();
    app.shutdown().await.unwrap();
    let app = ApplicationLifecycle::start(setup()).await.unwrap();
    assert!(app.presenter().presets.list().is_empty());
    assert_eq!(app.presenter().settings.presets.active.get(), id);
    app.shutdown().await.unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
}
