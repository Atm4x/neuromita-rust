use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, ChatApi},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse, ToolSettings,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    memory::{MemoryCommand, MemoryPriority},
    request::{
        ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState, UserInput,
    },
    settings::SettingsApi,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Call {
    input: GenerationInput,
    respond: oneshot::Sender<StructuredResponse>,
}

struct Provider(mpsc::UnboundedSender<Call>);
impl GenerationProvider for Provider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (respond, received) = oneshot::channel();
            self.0
                .send(Call { input, respond })
                .map_err(|_| GenerationError::Transport("mock receiver closed".into()))?;
            received
                .await
                .map_err(|_| GenerationError::Transport("mock response dropped".into()))
        })
    }
}

async fn start() -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
    let (send, recv) = mpsc::unbounded_channel();
    let template = GeminiTemplate::google(
        "fixture",
        "unused-key",
        GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let positive = |n| NonZeroUsize::new(n).unwrap();
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new([Arc::new(Provider(send)) as Arc<dyn GenerationProvider>]).unwrap(),
        RuntimeLimits {
            request_queue: positive(8),
            discovery_buffer: positive(16),
            character_commands: positive(16),
            presenter_updates: positive(16),
            retained_finished: positive(16),
        },
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    (app, recv)
}

fn reply() -> RequestHandle {
    let event =
        ChatApi::publish_message(CharacterId::Crazy, UserInput::new("test".into()).unwrap())
            .unwrap();
    ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        event,
        RequestSource::Presenter,
    ))
    .unwrap()
}

fn call(name: &str, args: serde_json::Value) -> StructuredResponse {
    let mut response = StructuredResponse::spoken("Проверю данные");
    response.tool_call = Some(neuromita::generation::ToolCall {
        name: name.into(),
        args: args.as_object().unwrap().clone(),
        native_origin: None,
    });
    response
}

async fn receive(rx: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    timeout(Duration::from_secs(8), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn finished(handle: &RequestHandle) -> Arc<neuromita::request::RequestSnapshot> {
    timeout(Duration::from_secs(8), handle.wait())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn calculator_continuation_only_accepts_final_response() {
    let _serial = SERIAL.lock().await;
    let (app, mut rx) = start().await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
    })
    .unwrap();
    let before = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    let request = reply();
    let first = receive(&mut rx).await;
    assert!(first.input.tool_history.is_empty());
    first
        .respond
        .send(call(
            "calculator",
            serde_json::json!({"expression":"2**3+1"}),
        ))
        .unwrap();
    let second = receive(&mut rx).await;
    assert_eq!(second.input.tool_history.len(), 2);
    assert!(second.input.tool_history.last().unwrap().text.contains("9"));
    let after_intermediate = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(before.accepted_replies, after_intermediate.accepted_replies);
    second
        .respond
        .send(StructuredResponse::spoken("Ответ: 9"))
        .unwrap();
    let snapshot = finished(&request).await;
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::Success(_))
        ),
        "{:?}",
        snapshot.state
    );
    assert_eq!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        before.accepted_replies + 1
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn disabled_and_intermediate_effects_fail_without_followup() {
    let _serial = SERIAL.lock().await;
    let (app, mut rx) = start().await;
    let request = reply();
    receive(&mut rx)
        .await
        .respond
        .send(call("calculator", serde_json::json!({"expression":"1+1"})))
        .unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
    })
    .unwrap();
    let request = reply();
    let mut intermediate = call("calculator", serde_json::json!({"expression":"2+2"}));
    intermediate.attitude_change = 10.0;
    receive(&mut rx).await.respond.send(intermediate).unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert!(rx.try_recv().is_err());
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn memory_search_reads_only_captured_character_facts() {
    let _serial = SERIAL.lock().await;
    let (app, mut rx) = start().await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
    })
    .unwrap();
    let presenter = &app.presenter().characters;
    presenter
        .edit_memories(
            CharacterId::Crazy,
            presenter.memories(CharacterId::Crazy).await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Мита любит вишнёвый чай".into(),
            }],
        )
        .await
        .unwrap();
    presenter
        .edit_memories(
            CharacterId::Kind,
            presenter.memories(CharacterId::Kind).await.unwrap(),
            vec![MemoryCommand::Add {
                priority: MemoryPriority::High,
                content: "Секретная история про вишнёвый чай".into(),
            }],
        )
        .await
        .unwrap();
    let request = reply();
    receive(&mut rx)
        .await
        .respond
        .send(call(
            "memory_search",
            serde_json::json!({"query":"вишнёвый чай","limit":5}),
        ))
        .unwrap();
    let second = receive(&mut rx).await;
    let result = second.input.tool_history.last().unwrap().text.as_ref();
    assert!(result.contains("Мита любит вишнёвый чай"), "{result}");
    assert!(!result.contains("Секретная история"), "{result}");
    second
        .respond
        .send(StructuredResponse::spoken("Нашла запись"))
        .unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn depth_limit_rejects_second_tool_without_acceptance() {
    let _serial = SERIAL.lock().await;
    let (app, mut rx) = start().await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
        tools.max_depth = 1;
    })
    .unwrap();
    let before = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    let request = reply();
    receive(&mut rx)
        .await
        .respond
        .send(call("calculator", serde_json::json!({"expression":"2+2"})))
        .unwrap();
    receive(&mut rx)
        .await
        .respond
        .send(call("calculator", serde_json::json!({"expression":"3+3"})))
        .unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert!(rx.try_recv().is_err());
    assert_eq!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        before.accepted_replies
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_tool_continuation_drops_unaccepted_result() {
    let _serial = SERIAL.lock().await;
    let (app, mut rx) = start().await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
    })
    .unwrap();
    let request = reply();
    receive(&mut rx)
        .await
        .respond
        .send(call("calculator", serde_json::json!({"expression":"1+2"})))
        .unwrap();
    let delayed = receive(&mut rx).await;
    assert!(
        delayed
            .input
            .tool_history
            .last()
            .unwrap()
            .text
            .contains("3")
    );
    let _ = request.cancel();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Cancelled
    ));
    assert!(
        delayed
            .respond
            .send(StructuredResponse::spoken("Late final"))
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_reader_denies_internal_target_without_followup_or_effects() {
    let _serial = SERIAL.lock().await;
    let (app, mut rx) = start().await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.web.enabled = true;
        tools.web.reader_enabled = true;
        tools.web.reader_allowed_hosts = vec!["docs.rust-lang.org".into()];
    })
    .unwrap();
    let before = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    let request = reply();
    receive(&mut rx)
        .await
        .respond
        .send(call(
            "web_reader",
            serde_json::json!({"url":"https://169.254.169.254/latest/meta-data/"}),
        ))
        .unwrap();
    assert!(matches!(
        finished(&request).await.state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert!(rx.try_recv().is_err());
    assert_eq!(
        CharacterApi::get(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        before.accepted_replies
    );
    app.shutdown().await.unwrap();
}
