use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, ChatApi},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, PromptRole,
        ProviderFuture, ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{
        ReplyRequest, RequestHandle, RequestOutcome, RequestSource, RequestState, UserInput,
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Call {
    input: GenerationInput,
    respond: oneshot::Sender<Result<StructuredResponse, GenerationError>>,
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
                .map_err(|_| GenerationError::Transport("mock response dropped".into()))?
        })
    }
}

async fn start() -> (ApplicationLifecycle, mpsc::UnboundedReceiver<Call>) {
    let (send, receive) = mpsc::unbounded_channel();
    let template = GeminiTemplate::google(
        "fixture",
        "unused-key",
        GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
    )
    .unwrap();
    let positive = |count| NonZeroUsize::new(count).unwrap();
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
    (app, receive)
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

async fn receive(calls: &mut mpsc::UnboundedReceiver<Call>) -> Call {
    timeout(Duration::from_secs(8), calls.recv())
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
async fn empty_spoken_response_is_repaired_once_without_duplicating_effects() {
    let _serial = SERIAL.lock().await;
    let (app, mut calls) = start().await;
    let before = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    let request = reply();

    let first = receive(&mut calls).await;
    assert!(first.input.tool_history.is_empty());
    first
        .respond
        .send(Err(GenerationError::EmptySpoken))
        .unwrap();

    let repaired = receive(&mut calls).await;
    assert_eq!(repaired.input.tool_history.len(), 1);
    assert_eq!(repaired.input.tool_history[0].role, PromptRole::User);
    assert!(
        repaired.input.tool_history[0]
            .text
            .contains("previous structured response contained no spoken text")
    );
    let mut final_response = StructuredResponse::spoken("Восстановлено после повтора");
    final_response.attitude_change = 2.0;
    repaired.respond.send(Ok(final_response)).unwrap();

    let snapshot = finished(&request).await;
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::Success(ref reply))
                if reply.text.as_ref() == "Восстановлено после повтора"
        ),
        "{:?}",
        snapshot.state
    );
    let after = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(after.attitude, before.attitude + 2.0);
    assert_eq!(after.accepted_replies, before.accepted_replies + 1);
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn second_empty_spoken_response_is_terminal_and_effect_free() {
    let _serial = SERIAL.lock().await;
    let (app, mut calls) = start().await;
    let before = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    let request = reply();

    receive(&mut calls)
        .await
        .respond
        .send(Err(GenerationError::EmptySpoken))
        .unwrap();
    receive(&mut calls)
        .await
        .respond
        .send(Err(GenerationError::EmptySpoken))
        .unwrap();

    let snapshot = finished(&request).await;
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(
                GenerationError::EmptySpoken
            ))
        ),
        "{:?}",
        snapshot.state
    );
    let after = CharacterApi::get(CharacterId::Crazy)
        .unwrap()
        .state()
        .await
        .unwrap();
    assert_eq!(after.attitude, before.attitude);
    assert_eq!(after.accepted_replies, before.accepted_replies);
    assert!(calls.try_recv().is_err());
    app.shutdown().await.unwrap();
}
