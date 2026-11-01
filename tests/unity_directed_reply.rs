use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterContext, CharacterId},
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderFuture, ProviderKind,
        ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{ReplyRequest, RequestOutcome, RequestSource, RequestState, SubmitError, UserInput},
    timeline::{
        EventContent, TimelineConfig,
        conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker},
        domain::{BranchId, EventKind, SessionId},
        ingress::{SemanticBatch, SemanticEvent},
    },
};
use serde_json::json;
use uuid::Uuid;

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

struct LlmCall {
    context: CharacterContext,
    reply: oneshot::Sender<Arc<str>>,
}

struct ControlledProvider(mpsc::UnboundedSender<LlmCall>);

impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(
        &self,
        context: CharacterContext,
        _template: ProviderTemplate,
    ) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(LlmCall { context, reply })
                .map_err(|_| GenerationError::Transport("test provider stopped".into()))?;
            receive
                .await
                .map_err(|_| GenerationError::Transport("test response dropped".into()))
        })
    }
}

fn preset() -> GenerationPreset {
    GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "test-model",
            "test-key",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ))
}

fn config(sender: mpsc::UnboundedSender<LlmCall>) -> ApplicationConfig {
    let provider: Arc<dyn GenerationProvider> = Arc::new(ControlledProvider(sender));
    ApplicationConfig::minimal(
        preset(),
        ResponseGenerator::new([provider]).unwrap(),
        RuntimeLimits {
            request_queue: NonZeroUsize::new(4).unwrap(),
            discovery_buffer: NonZeroUsize::new(8).unwrap(),
            character_commands: NonZeroUsize::new(8).unwrap(),
            presenter_updates: NonZeroUsize::new(8).unwrap(),
            retained_finished: NonZeroUsize::new(8).unwrap(),
        },
        "127.0.0.1:0".parse().unwrap(),
    )
}

fn character_message(text: &str, witnessed_by: &[CharacterId]) -> ConversationMessage {
    let mut message = ConversationMessage::new(
        "unity:dialogue:source",
        ConversationSpeaker::Character(CharacterId::Mita),
        json!(text),
        witnessed_by.iter().copied(),
    );
    message
        .audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::KindMita));
    message.audience.participants.extend([
        ConversationSpeaker::Character(CharacterId::Mita),
        ConversationSpeaker::Character(CharacterId::KindMita),
    ]);
    message
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unity_decides_whether_an_observed_character_message_receives_a_reply() {
    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let source = character_message("KindMita, are you ready?", &[CharacterId::KindMita]);
    let source_id = source.id;
    app.publish_conversation_message(app.timeline_selection(), source)
        .unwrap();
    assert_eq!(app.at().through, 1);
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );

    let mut audience = ConversationAudience::private_to(CharacterId::KindMita);
    audience
        .targets
        .insert(ConversationSpeaker::Character(CharacterId::Mita));
    audience.participants.extend([
        ConversationSpeaker::Character(CharacterId::Mita),
        ConversationSpeaker::Character(CharacterId::KindMita),
    ]);
    audience.seen_by.insert(CharacterId::Mita);

    let handle = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::KindMita,
            preset(),
            UserInput::new("Answer the last line, following Unity's turn decision".into()).unwrap(),
            RequestSource::Presenter,
        )
        .to_event(source_id)
        .with_audience(audience),
    )
    .unwrap();

    let generated = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(generated.context.view.count(), 1);
    assert_eq!(generated.context.view.messages()[0].event_id, source_id);
    assert_eq!(
        generated.context.current_input.as_ref(),
        "Answer the last line, following Unity's turn decision"
    );
    generated.reply.send(Arc::from("Ready, Mita!")).unwrap();

    let completed = timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    let RequestState::Finished(RequestOutcome::Success(accepted)) = &completed.state else {
        panic!("expected successful generated reply: {:?}", completed.state);
    };
    let accepted_id = accepted.event_id;
    let view_mita = app
        .character(CharacterId::Mita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    let view_kind = app
        .character(CharacterId::KindMita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(view_mita.view.count(), 2);
    assert_eq!(view_kind.view.count(), 2);
    assert_eq!(view_mita.view.messages()[1].event_id, accepted_id);
    assert_eq!(view_mita.view.provider_messages()[1]["role"], "user");
    assert_eq!(view_kind.view.provider_messages()[1]["role"], "assistant");

    let events = app.timeline();
    assert_eq!(app.at().through, 3);
    assert_eq!(events.len(), 1);
    assert!(!events.iter().any(|event| matches!(
        event.content,
        neuromita::timeline::EventContent::PlayerMessage { .. }
    )));
    assert!(events.iter().any(|event| match &event.content {
        neuromita::timeline::EventContent::CharacterReply { reply_to, .. } =>
            *reply_to == source_id,
        _ => false,
    }));
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_requires_a_target_known_to_the_selected_character() {
    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let source = character_message("Private", &[]);
    let source_id = source.id;
    app.publish_conversation_message(app.timeline_selection(), source)
        .unwrap();
    let result = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::KindMita,
            preset(),
            UserInput::new("reply".into()).unwrap(),
            RequestSource::Presenter,
        )
        .to_event(source_id),
    );
    assert!(matches!(result, Err(SubmitError::InvalidReplyTarget)));
    assert_eq!(app.at().through, 1);
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_reply_can_target_an_observed_unity_event() {
    let _lock = TEST_LOCK.lock().await;
    let (sent, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(sent)).await.unwrap();
    let mut source = SemanticEvent::new(EventKind::UnityEvent, [CharacterId::KindMita]);
    let source_id = source.id;
    source.payload.insert("name".into(), json!("DoorOpened"));
    app.publish_semantic_events(
        app.timeline_selection(),
        SemanticBatch::new("unity:door-open", vec![source]),
    )
    .unwrap();
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );

    let handle = ChatApi::reply(
        ReplyRequest::new(
            CharacterId::KindMita,
            preset(),
            UserInput::new("React verbally to the observed door opening".into()).unwrap(),
            RequestSource::Presenter,
        )
        .to_event(source_id),
    )
    .unwrap();
    let generated = timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(generated.context.view.world_events().len(), 1);
    assert_eq!(generated.context.view.world_events()[0].event_id, source_id);
    generated
        .reply
        .send(Arc::from("Who opened the door?"))
        .unwrap();
    let completed = timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        completed.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let events = app.timeline();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.content, EventContent::PlayerMessage { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event.content,
        EventContent::CharacterReply { reply_to, .. } if reply_to == source_id
    )));
    assert!(
        timeout(Duration::from_millis(50), calls.recv())
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unity_directed_reply_survives_sqlite_restart_without_player_message() {
    let _lock = TEST_LOCK.lock().await;
    let path = std::env::temp_dir().join(format!("neuromita-directed-reply-{}.db", Uuid::new_v4()));
    let session = SessionId::new();
    let root = BranchId::new();
    let source = character_message("KindMita, over here!", &[CharacterId::KindMita]);
    let source_id = source.id;
    let reply_id = {
        let (sent, mut calls) = mpsc::unbounded_channel();
        let mut configuration = config(sent);
        configuration.timeline = TimelineConfig::Sqlite {
            database: path.clone(),
            session,
            root,
        };
        let app = ApplicationLifecycle::start(configuration).await.unwrap();
        app.publish_conversation_message(app.timeline_selection(), source)
            .unwrap();
        let handle = ChatApi::reply(
            ReplyRequest::new(
                CharacterId::KindMita,
                preset(),
                UserInput::new("Unity requests a reply to Mita".into()).unwrap(),
                RequestSource::Presenter,
            )
            .to_event(source_id),
        )
        .unwrap();
        let call = timeout(Duration::from_secs(5), calls.recv())
            .await
            .unwrap()
            .unwrap();
        call.reply.send(Arc::from("Coming!")).unwrap();
        let finished = timeout(Duration::from_secs(5), handle.wait())
            .await
            .unwrap()
            .unwrap();
        let RequestState::Finished(RequestOutcome::Success(accepted)) = &finished.state else {
            panic!("reply did not complete: {:?}", finished.state);
        };
        let id = accepted.event_id;
        app.shutdown().await.unwrap();
        id
    };
    let (sent, _calls) = mpsc::unbounded_channel();
    let mut configuration = config(sent);
    configuration.timeline = TimelineConfig::Sqlite {
        database: path.clone(),
        session,
        root,
    };
    let app = ApplicationLifecycle::start(configuration).await.unwrap();
    let events = app.timeline();
    assert!(events.iter().any(|event| event.id == reply_id
        && matches!(
            event.content,
            EventContent::CharacterReply { reply_to, .. } if reply_to == source_id
        )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.content, EventContent::PlayerMessage { .. }))
    );
    let view = app
        .character(CharacterId::KindMita)
        .unwrap()
        .get_context(app.at())
        .await
        .unwrap();
    assert_eq!(view.view.messages().len(), 2);
    assert_eq!(view.view.messages()[1].event_id, reply_id);
    app.shutdown().await.unwrap();
    for suffix in ["", "-shm", "-wal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
