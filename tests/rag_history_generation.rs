use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::KnowledgeRetrieval,
    request::{
        InitiateRequest, ReplyRequest, RequestOutcome, RequestSource, RequestState, UserInput,
    },
    settings::{HistoryGenerationMode, HistoryGenerationSettings, SettingsApi},
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

static ONE_APP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ProviderCall {
    input: GenerationInput,
    reply: oneshot::Sender<String>,
}
struct ControlledProvider(mpsc::UnboundedSender<ProviderCall>);
impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(ProviderCall { input, reply })
                .map_err(|_| GenerationError::Transport("provider observation closed".into()))?;
            let text = receive
                .await
                .map_err(|_| GenerationError::Transport("provider cancelled".into()))?;
            Ok(StructuredResponse::spoken(text))
        })
    }
}
fn config(sender: mpsc::UnboundedSender<ProviderCall>) -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "test",
            "unused",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let size = |n| NonZeroUsize::new(n).unwrap();
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([
            Arc::new(ControlledProvider(sender)) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        RuntimeLimits {
            request_queue: size(4),
            discovery_buffer: size(8),
            character_commands: size(16),
            presenter_updates: size(8),
            retained_finished: size(8),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.knowledge = KnowledgeRetrieval::lexical();
    config
}
async fn next_call(receiver: &mut mpsc::UnboundedReceiver<ProviderCall>) -> ProviderCall {
    timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn generation_history_is_opt_in_captured_at_admission_and_scoped_to_live_character() {
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    assert_eq!(
        SettingsApi::snapshot::<HistoryGenerationSettings>()
            .unwrap()
            .value
            .mode,
        HistoryGenerationMode::Off
    );
    let first = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Jasmine tea is only for Crazy".into()).unwrap(),
    )
    .unwrap();
    let handle = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        first,
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    assert!(
        !call
            .input
            .prepared_knowledge
            .as_deref()
            .unwrap_or("")
            .contains("[RETRIEVED TIMELINE EVIDENCE]")
    );
    call.reply.send("Okay".into()).unwrap();
    let finished = timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        finished.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let previous = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    let selected = HistoryGenerationSettings {
        mode: HistoryGenerationMode::Keyword,
        active_messages: 1,
        ..previous.value
    };
    SettingsApi::replace(previous.revision, selected).unwrap();
    let handle = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Jasmine tea",
        RequestSource::Presenter,
    ))
    .unwrap();
    let admitted = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        admitted.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Off,
            ..admitted.value
        },
    )
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(
        evidence.contains("[RETRIEVED TIMELINE EVIDENCE]"),
        "{evidence}"
    );
    assert!(evidence.contains(&first.0.to_string()));
    assert!(evidence.contains("Jasmine tea is only for Crazy"));
    assert!(!evidence.contains("[FORGOTTEN]"));
    let window = call.input.history_window.as_ref().unwrap();
    assert!(window.is_archived(first));
    assert!(!window.is_active(first));
    call.reply.send("Confirmed".into()).unwrap();
    let finished = timeout(Duration::from_secs(5), handle.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        finished.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let on = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        on.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Keyword,
            ..on.value
        },
    )
    .unwrap();
    let other = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Kind,
        "tea",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    assert!(
        !call
            .input
            .prepared_knowledge
            .as_deref()
            .unwrap_or("")
            .contains("Jasmine tea is only for Crazy")
    );
    call.reply.send("No private facts".into()).unwrap();
    let result = timeout(Duration::from_secs(5), other.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn vector_generation_requires_backend_and_does_not_silently_fall_back() {
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            ..settings.value
        },
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Jasmine tea".into()).unwrap(),
    )
    .unwrap();
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "jasmine tea",
        RequestSource::Presenter,
    ))
    .unwrap();
    let finished = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            finished.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
        ),
        "{:?}",
        finished.state
    );
    assert!(
        calls.try_recv().is_err(),
        "provider invoked after unavailable vector backend"
    );
    app.shutdown().await.unwrap();
}

#[test]
fn generation_history_defaults_off_and_rejects_invalid_budgets() {
    let settings = HistoryGenerationSettings::default();
    assert_eq!(settings.mode, HistoryGenerationMode::Off);
    assert!(settings.validate().is_ok());
    assert!(
        HistoryGenerationSettings {
            max_results: 0,
            ..settings
        }
        .validate()
        .is_err()
    );
    assert!(
        HistoryGenerationSettings {
            max_content_chars: 4001,
            ..settings
        }
        .validate()
        .is_err()
    );
    assert!(
        HistoryGenerationSettings {
            active_messages: 0,
            ..settings
        }
        .validate()
        .is_err()
    );
    assert!(
        HistoryGenerationSettings {
            active_messages: 65,
            ..settings
        }
        .validate()
        .is_err()
    );
    let deserialized: HistoryGenerationSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(deserialized, settings);
}

#[tokio::test]
async fn timeline_change_before_provider_reply_rejects_selected_history_at_acceptance() {
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Keyword,
            ..settings.value
        },
    )
    .unwrap();
    let first = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Tea on the balcony".into()).unwrap(),
    )
    .unwrap();
    let request = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        first,
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    assert!(call.input.history_window.is_some());
    assert!(
        call.input
            .prepared_knowledge
            .as_deref()
            .unwrap_or("")
            .find("[RETRIEVED TIMELINE EVIDENCE]")
            .is_none()
    );
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Context changed before response".into()).unwrap(),
    )
    .unwrap();
    let _ = call.reply.send("must not accept".into());
    let finished = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !matches!(
            finished.state,
            RequestState::Finished(RequestOutcome::Success(_))
        ),
        "{:?}",
        finished.state
    );
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn user_selected_fts_reaches_the_live_provider_with_scoped_bm25_evidence() {
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Fts,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let event = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Jasmine and pine needles".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Today's plans".into()).unwrap(),
    )
    .unwrap();
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Jasmine pine",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&event.0.to_string()));
    assert!(evidence.contains("Jasmine and pine needles"));
    call.reply.send("Found".into()).unwrap();
    let result = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            result.state,
            RequestState::Finished(RequestOutcome::Success(_))
        ),
        "{:?}",
        result.state
    );
    app.shutdown().await.unwrap();
}

struct PausedEmbedding {
    space: neuromita::knowledge::embedding::EmbeddingSpace,
    started: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}
impl neuromita::knowledge::embedding::EmbeddingProvider for PausedEmbedding {
    fn space(&self) -> &neuromita::knowledge::embedding::EmbeddingSpace {
        &self.space
    }
    fn embed(
        &self,
        purpose: neuromita::knowledge::embedding::EmbeddingPurpose,
        _: String,
    ) -> neuromita::knowledge::embedding::EmbeddingFuture<
        '_,
        neuromita::knowledge::embedding::Embedding,
    > {
        Box::pin(async move {
            if purpose == neuromita::knowledge::embedding::EmbeddingPurpose::Query {
                self.started.notify_one();
                self.resume.notified().await;
            }
            neuromita::knowledge::embedding::Embedding::new(vec![1.0, 0.0], &self.space)
        })
    }
}

#[tokio::test]
async fn changing_timeline_during_vector_search_cancels_before_provider_invocation() {
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let provider = Arc::new(PausedEmbedding {
        space: neuromita::knowledge::embedding::EmbeddingSpace::new("fixture", "paused", "v1", 2)
            .unwrap(),
        started: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    });
    let mut config = config(tx);
    config.knowledge = KnowledgeRetrieval::semantic(Arc::new(
        neuromita::knowledge::semantic_memory::SemanticMemoryRetriever::new(provider.clone(), 16)
            .unwrap(),
    ));
    let app = ApplicationLifecycle::start(config).await.unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let _source = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Tea on balcony".into()).unwrap(),
    )
    .unwrap();
    let current = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("New topic before search".into()).unwrap(),
    )
    .unwrap();
    let request = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        current,
        RequestSource::Presenter,
    ))
    .unwrap();
    timeout(Duration::from_secs(5), provider.started.notified())
        .await
        .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("New Timeline revision".into()).unwrap(),
    )
    .unwrap();
    provider.resume.notify_one();
    let finished = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !matches!(
            finished.state,
            RequestState::Finished(RequestOutcome::Success(_))
        ),
        "{:?}",
        finished.state
    );
    assert!(calls.try_recv().is_err(), "LLM saw stale vector evidence");
    assert_eq!(
        app.character(CharacterId::Crazy)
            .unwrap()
            .state()
            .await
            .unwrap()
            .accepted_replies,
        0
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn generation_renderer_rejects_forged_content_and_cross_character_source_identity() {
    use neuromita::knowledge::{
        history_generation::{HistoryEvidenceError, render_history_evidence},
        history_retrieval::{HistorySearchConfig, search_history},
    };
    let _lock = ONE_APP.lock().await;
    let (tx, _) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("My book is in the library".into()).unwrap(),
    )
    .unwrap();
    let owner = app.character(CharacterId::Crazy).unwrap();
    let context = owner.get_context(app.at()).await.unwrap();
    let result = search_history(
        CharacterId::Crazy,
        &context.view,
        "library",
        HistorySearchConfig::default(),
    )
    .unwrap();
    assert!(
        render_history_evidence(CharacterId::Crazy, &context.view, &result, 4, 1200)
            .unwrap()
            .unwrap()
            .contains("My book")
    );
    let mut changed = result;
    changed.hits[0].content = "forged message".into();
    assert_eq!(
        render_history_evidence(CharacterId::Crazy, &context.view, &changed, 4, 1200),
        Err(HistoryEvidenceError::InvalidEvidence)
    );
    changed.hits[0].content = "My book is in the library".into();
    changed.hits[0].key.character = CharacterId::Kind;
    assert_eq!(
        render_history_evidence(CharacterId::Crazy, &context.view, &changed, 4, 1200),
        Err(HistoryEvidenceError::InvalidEvidence)
    );
    assert_eq!(
        render_history_evidence(CharacterId::Kind, &context.view, &changed, 4, 1200),
        Err(HistoryEvidenceError::StaleSelection)
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn lexical_generation_fuses_memory_and_archived_history_under_one_result_budget() {
    use neuromita::{
        api::CharacterApi,
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let first_memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Jasmine tea is the preferred evening drink",
    );
    let second_memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Low,
        "Jasmine tea is available in the pantry",
    );
    let selected_memory_id = first_memory.id;
    let excluded_memory_id = second_memory.id;
    owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![
                MemoryCommand::ImportRecord {
                    record: first_memory,
                },
                MemoryCommand::ImportRecord {
                    record: second_memory,
                },
            ],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Jasmine tea was shared on the balcony".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Current unrelated topic".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Keyword,
            max_results: 2,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Jasmine",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(
        evidence.contains(&selected_memory_id.0.to_string()),
        "{evidence}"
    );
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(
        !evidence.contains(&excluded_memory_id.0.to_string()),
        "{evidence}"
    );
    assert_eq!(evidence.matches("[LONG-TERM MEMORY]").count(), 1);
    assert_eq!(evidence.matches("[RETRIEVED TIMELINE EVIDENCE]").count(), 1);
    assert!(
        call.input
            .history_window
            .as_ref()
            .unwrap()
            .is_archived(archived)
    );
    call.reply.send("Both selected".into()).unwrap();
    let done = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        done.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn fts_generation_fuses_memory_and_archived_history_without_keyword_fallback() {
    use neuromita::{
        api::CharacterApi,
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::High,
        "Orchid flower is growing near the window",
    );
    let id = memory.id;
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord { record: memory }],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Orchid flower has bloomed yesterday".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Today we travel".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Fts,
            max_results: 2,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Orchid",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    call.reply.send("Found both".into()).unwrap();
    let done = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        done.state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn reply_and_react_share_lexical_memory_history_admission_without_cross_character_evidence() {
    use neuromita::{
        api::CharacterApi,
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
        request::ReactRequest,
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::High,
        "Pear orchard belongs to Crazy",
    );
    let memory_id = memory.id;
    owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord { record: memory }],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Pear orchard grew in spring".into()).unwrap(),
    )
    .unwrap();
    let target = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("What happened to the orchard?".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Keyword,
            max_results: 2,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();

    let reply = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        target,
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&memory_id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(!evidence.contains(&target.0.to_string()), "{evidence}");
    call.reply.send("Remembered".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), reply.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let react = ChatApi::react(ReactRequest::new(
        CharacterId::Crazy,
        target,
        Arc::clone(
            UserInput::new("Remember the pear orchard".into())
                .unwrap()
                .text(),
        ),
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&memory_id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(!evidence.contains(&target.0.to_string()), "{evidence}");
    call.reply.send("Acknowledged".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), react.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}
