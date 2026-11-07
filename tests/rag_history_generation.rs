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

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn fts_generation_uses_scoped_unicode61_for_memory_and_history_and_excludes_hidden() {
    use neuromita::{
        api::CharacterApi,
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let active = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Normal,
        "A quiet café beside the river",
    );
    let mut rag_only = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Normal,
        "The café serves breakfast at dawn",
    );
    rag_only.kind = neuromita::memory::MemoryKind::RagOnly;
    let rag_only_id = rag_only.id;
    let hidden = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::High,
        "A forgotten café on the hill",
    );
    let unrelated = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "A mountain without any matching terms",
    );
    let active_id = active.id;
    let hidden_id = hidden.id;
    let unrelated_id = unrelated.id;
    let result = owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![
                MemoryCommand::ImportRecord { record: active },
                MemoryCommand::ImportRecord { record: rag_only },
                MemoryCommand::ImportRecord { record: hidden },
                MemoryCommand::ImportRecord { record: unrelated },
            ],
        )
        .await
        .unwrap();
    owner
        .edit_memories(
            result.snapshot,
            vec![MemoryCommand::Forget { id: hidden_id }],
        )
        .await
        .unwrap();
    let other = CharacterApi::get(CharacterId::Kind).unwrap();
    let other_memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Another character's café",
    );
    let other_id = other_memory.id;
    other
        .edit_memories(
            other.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord {
                record: other_memory,
            }],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("The cafe opened yesterday".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("A new morning begins".into()).unwrap(),
    )
    .unwrap();

    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Fts,
            max_results: 3,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "cafe",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&active_id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&rag_only_id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(!evidence.contains(&hidden_id.0.to_string()), "{evidence}");
    assert!(
        !evidence.contains(&unrelated_id.0.to_string()),
        "{evidence}"
    );
    assert!(!evidence.contains(&other_id.0.to_string()), "{evidence}");
    call.reply
        .send("Found archived source evidence".into())
        .unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
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

struct JointEmbedding {
    space: neuromita::knowledge::embedding::EmbeddingSpace,
    queries: std::sync::atomic::AtomicUsize,
    documents: std::sync::atomic::AtomicUsize,
}

impl JointEmbedding {
    fn new() -> Self {
        Self {
            space: neuromita::knowledge::embedding::EmbeddingSpace::new(
                "fixture",
                "joint-generation",
                "v1",
                2,
            )
            .unwrap(),
            queries: std::sync::atomic::AtomicUsize::new(0),
            documents: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl neuromita::knowledge::embedding::EmbeddingProvider for JointEmbedding {
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
                self.queries
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            neuromita::knowledge::embedding::Embedding::new(vec![1.0, 0.0], &self.space)
        })
    }

    fn embed_documents(
        &self,
        texts: Vec<String>,
    ) -> neuromita::knowledge::embedding::EmbeddingFuture<
        '_,
        Vec<neuromita::knowledge::embedding::Embedding>,
    > {
        Box::pin(async move {
            self.documents
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            texts
                .into_iter()
                .map(|_| {
                    neuromita::knowledge::embedding::Embedding::new(vec![1.0, 0.0], &self.space)
                })
                .collect()
        })
    }
}

struct JointReranker {
    calls: std::sync::atomic::AtomicUsize,
    candidate_count: std::sync::atomic::AtomicUsize,
    failure: bool,
}

impl neuromita::knowledge::reranking::RerankProvider for JointReranker {
    fn score<'a>(
        &'a self,
        _: &'a str,
        texts: &'a [String],
    ) -> neuromita::knowledge::reranking::RerankFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.candidate_count
                .store(texts.len(), std::sync::atomic::Ordering::SeqCst);
            if self.failure {
                return Err(neuromita::knowledge::embedding::EmbeddingError::Transport(
                    "joint reranker offline".into(),
                ));
            }
            Ok(texts
                .iter()
                .map(|text| if text.contains("Gallery") { 0.99 } else { 0.1 })
                .collect())
        })
    }
}

#[tokio::test]
async fn joint_vector_generation_uses_one_embedding_query_and_one_cross_source_rerank() {
    use neuromita::{
        api::CharacterApi,
        knowledge::{reranking::RerankPolicy, semantic_memory::SemanticMemoryRetriever},
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };
    use std::sync::atomic::Ordering;
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let embedding = Arc::new(JointEmbedding::new());
    let rerank = Arc::new(JointReranker {
        calls: std::sync::atomic::AtomicUsize::new(0),
        candidate_count: std::sync::atomic::AtomicUsize::new(0),
        failure: false,
    });
    let mut application = config(tx);
    application.knowledge = KnowledgeRetrieval::semantic(Arc::new(
        SemanticMemoryRetriever::new(embedding.clone(), 64)
            .unwrap()
            .with_reranker(
                rerank.clone(),
                RerankPolicy {
                    alpha: 1.0,
                    limit: 4,
                },
            )
            .unwrap(),
    ));
    let app = ApplicationLifecycle::start(application).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Orchid belongs to memory",
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
        UserInput::new("Gallery contains the archived orchid photograph".into()).unwrap(),
    )
    .unwrap();
    let active = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("This is the current unrelated message".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            max_results: 2,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let request = ChatApi::reply(ReplyRequest::new(
        CharacterId::Crazy,
        active,
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&memory_id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(!evidence.contains(&active.0.to_string()), "{evidence}");
    assert_eq!(embedding.queries.load(Ordering::SeqCst), 1);
    assert_eq!(embedding.documents.load(Ordering::SeqCst), 1);
    assert_eq!(rerank.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rerank.candidate_count.load(Ordering::SeqCst), 2);
    call.reply.send("Both".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn joint_fusion_one_budget_reranker_selects_history_over_memory_and_rejects_failed_provider()
{
    use neuromita::{
        api::CharacterApi,
        knowledge::{
            reranking::RerankPolicy, retrieval_fusion::FusionMode,
            semantic_memory::SemanticMemoryRetriever,
        },
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };
    use std::sync::atomic::Ordering;
    let _lock = ONE_APP.lock().await;
    for fail in [false, true] {
        let (tx, mut calls) = mpsc::unbounded_channel();
        let embedding = Arc::new(JointEmbedding::new());
        let rerank = Arc::new(JointReranker {
            calls: std::sync::atomic::AtomicUsize::new(0),
            candidate_count: std::sync::atomic::AtomicUsize::new(0),
            failure: fail,
        });
        let mut application = config(tx);
        application.knowledge = KnowledgeRetrieval::semantic_fusion(
            Arc::new(
                SemanticMemoryRetriever::new(embedding.clone(), 64)
                    .unwrap()
                    .with_reranker(
                        rerank.clone(),
                        RerankPolicy {
                            alpha: 1.0,
                            limit: 4,
                        },
                    )
                    .unwrap(),
            ),
            FusionMode::Union,
        )
        .unwrap();
        let app = ApplicationLifecycle::start(application).await.unwrap();
        let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
        let memory = MemoryRecord::fact(
            MemoryId::new(),
            MemoryPriority::Critical,
            "Orchid belongs to memory",
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
            UserInput::new("Gallery contains the archived orchid photograph".into()).unwrap(),
        )
        .unwrap();
        ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("An unrelated active message".into()).unwrap(),
        )
        .unwrap();
        let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
        SettingsApi::replace(
            settings.revision,
            HistoryGenerationSettings {
                mode: HistoryGenerationMode::Fusion,
                max_results: 1,
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
        if fail {
            let done = timeout(Duration::from_secs(5), request.wait())
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(
                    done.state,
                    RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
                ),
                "{:?}",
                done.state
            );
            assert!(
                calls.try_recv().is_err(),
                "failed reranker leaked an LLM request"
            );
        } else {
            let call = next_call(&mut calls).await;
            let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
            assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
            assert!(!evidence.contains(&memory_id.0.to_string()), "{evidence}");
            assert_eq!(evidence.matches("[RETRIEVED TIMELINE EVIDENCE]").count(), 1);
            call.reply.send("Chosen".into()).unwrap();
            assert!(matches!(
                timeout(Duration::from_secs(5), request.wait())
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                RequestState::Finished(RequestOutcome::Success(_))
            ));
        }
        assert_eq!(embedding.queries.load(Ordering::SeqCst), 1);
        assert_eq!(rerank.calls.load(Ordering::SeqCst), 1);
        app.shutdown().await.unwrap();
    }
}

struct PausedJointReranker {
    started: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
}

impl neuromita::knowledge::reranking::RerankProvider for PausedJointReranker {
    fn score<'a>(
        &'a self,
        _: &'a str,
        texts: &'a [String],
    ) -> neuromita::knowledge::reranking::RerankFuture<'a> {
        Box::pin(async move {
            self.started.notify_one();
            self.resume.notified().await;
            Ok(vec![0.5; texts.len()])
        })
    }
}

#[tokio::test]
async fn timeline_revision_change_during_joint_rerank_cancels_before_llm_admission() {
    use neuromita::{
        api::CharacterApi,
        knowledge::{reranking::RerankPolicy, semantic_memory::SemanticMemoryRetriever},
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let reranker = Arc::new(PausedJointReranker {
        started: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    });
    let mut application = config(tx);
    application.knowledge = KnowledgeRetrieval::semantic(Arc::new(
        SemanticMemoryRetriever::new(Arc::new(JointEmbedding::new()), 64)
            .unwrap()
            .with_reranker(
                reranker.clone(),
                RerankPolicy {
                    alpha: 0.5,
                    limit: 3,
                },
            )
            .unwrap(),
    ));
    let app = ApplicationLifecycle::start(application).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord {
                record: MemoryRecord::fact(MemoryId::new(), MemoryPriority::High, "Orchid memory"),
            }],
        )
        .await
        .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Gallery archived orchid".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Active topic".into()).unwrap(),
    )
    .unwrap();
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
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Orchid",
        RequestSource::Presenter,
    ))
    .unwrap();
    timeout(Duration::from_secs(5), reranker.started.notified())
        .await
        .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Timeline changed during joint reranking".into()).unwrap(),
    )
    .unwrap();
    reranker.resume.notify_one();
    let done = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !matches!(
            done.state,
            RequestState::Finished(RequestOutcome::Success(_))
        ),
        "{:?}",
        done.state
    );
    assert!(
        calls.try_recv().is_err(),
        "stale History/Memory selection leaked to LLM"
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn joint_generation_retains_memory_intersection_policy_without_suppressing_history() {
    use neuromita::{
        api::CharacterApi,
        knowledge::{retrieval_fusion::FusionMode, semantic_memory::SemanticMemoryRetriever},
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
    };
    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let mut application = config(tx);
    application.knowledge = KnowledgeRetrieval::semantic_fusion(
        Arc::new(SemanticMemoryRetriever::new(Arc::new(JointEmbedding::new()), 32).unwrap()),
        FusionMode::Intersect {
            min_methods: 2,
            require_vector: true,
        },
    )
    .unwrap();
    let app = ApplicationLifecycle::start(application).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Unrelated orchard memory",
    );
    let ignored_id = memory.id;
    owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord { record: memory }],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Gallery has the antique orchid".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Active topic".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            max_results: 1,
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
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(!evidence.contains(&ignored_id.0.to_string()), "{evidence}");
    call.reply.send("Selected".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn joint_semantic_react_uses_one_scoped_memory_history_selection() {
    use neuromita::{
        api::CharacterApi,
        knowledge::{reranking::RerankPolicy, semantic_memory::SemanticMemoryRetriever},
        memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
        request::ReactRequest,
    };
    use std::sync::atomic::Ordering;

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let embedding = Arc::new(JointEmbedding::new());
    let reranker = Arc::new(JointReranker {
        calls: std::sync::atomic::AtomicUsize::new(0),
        candidate_count: std::sync::atomic::AtomicUsize::new(0),
        failure: false,
    });
    let mut application = config(tx);
    application.knowledge = KnowledgeRetrieval::semantic(Arc::new(
        SemanticMemoryRetriever::new(embedding.clone(), 64)
            .unwrap()
            .with_reranker(
                reranker.clone(),
                RerankPolicy {
                    alpha: 0.75,
                    limit: 4,
                },
            )
            .unwrap(),
    ));
    let app = ApplicationLifecycle::start(application).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let memory = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::High,
        "Orchid exhibit is a private memory",
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
        UserInput::new("Gallery contains the original orchid photograph".into()).unwrap(),
    )
    .unwrap();
    let active = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("React to the orchid photograph".into()).unwrap(),
    )
    .unwrap();
    let previous = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        previous.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            max_results: 2,
            active_messages: 1,
            ..previous.value
        },
    )
    .unwrap();
    let request = ChatApi::react(ReactRequest::new(
        CharacterId::Crazy,
        active,
        Arc::clone(
            UserInput::new("Orchid gallery memory".into())
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
    assert!(!evidence.contains(&active.0.to_string()), "{evidence}");
    assert_eq!(embedding.queries.load(Ordering::SeqCst), 1);
    assert_eq!(embedding.documents.load(Ordering::SeqCst), 1);
    assert_eq!(reranker.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reranker.candidate_count.load(Ordering::SeqCst), 2);
    call.reply.send("Acknowledged".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

struct InvalidScoreReranker(Vec<f64>);

impl neuromita::knowledge::reranking::RerankProvider for InvalidScoreReranker {
    fn score<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
    ) -> neuromita::knowledge::reranking::RerankFuture<'a> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

#[tokio::test]
async fn joint_generation_rejects_nonfinite_out_of_range_and_unaligned_reranker_scores() {
    use neuromita::knowledge::{reranking::RerankPolicy, semantic_memory::SemanticMemoryRetriever};

    let _lock = ONE_APP.lock().await;
    for scores in [
        vec![],
        vec![f64::NAN],
        vec![f64::INFINITY],
        vec![-0.1],
        vec![1.1],
    ] {
        let (tx, mut calls) = mpsc::unbounded_channel();
        let mut application = config(tx);
        application.knowledge = KnowledgeRetrieval::semantic(Arc::new(
            SemanticMemoryRetriever::new(Arc::new(JointEmbedding::new()), 32)
                .unwrap()
                .with_reranker(
                    Arc::new(InvalidScoreReranker(scores)),
                    RerankPolicy {
                        alpha: 1.0,
                        limit: 4,
                    },
                )
                .unwrap(),
        ));
        let app = ApplicationLifecycle::start(application).await.unwrap();
        ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Archived orchid gallery".into()).unwrap(),
        )
        .unwrap();
        ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Active message".into()).unwrap(),
        )
        .unwrap();
        let previous = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
        SettingsApi::replace(
            previous.revision,
            HistoryGenerationSettings {
                mode: HistoryGenerationMode::Vector,
                active_messages: 1,
                ..previous.value
            },
        )
        .unwrap();
        let request = ChatApi::initiate(InitiateRequest::new(
            CharacterId::Crazy,
            "Orchid",
            RequestSource::Presenter,
        ))
        .unwrap();
        let done = timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                done.state,
                RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
            ),
            "{:?}",
            done.state
        );
        assert!(
            calls.try_recv().is_err(),
            "malformed reranker score leaked to LLM"
        );
        app.shutdown().await.unwrap();
    }
}

#[derive(Clone, Copy)]
enum InvalidEmbeddingCase {
    QueryNonfinite,
    QueryWrongSpace,
    DocumentNonfinite,
    DocumentWrongSpace,
    DocumentCountMismatch,
}

struct InvalidJointEmbedding {
    space: neuromita::knowledge::embedding::EmbeddingSpace,
    case: InvalidEmbeddingCase,
}

impl InvalidJointEmbedding {
    fn new(case: InvalidEmbeddingCase) -> Self {
        Self {
            space: neuromita::knowledge::embedding::EmbeddingSpace::new(
                "fixture",
                "invalid-joint-generation",
                "v1",
                2,
            )
            .unwrap(),
            case,
        }
    }
}

impl neuromita::knowledge::embedding::EmbeddingProvider for InvalidJointEmbedding {
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
        use neuromita::knowledge::embedding::{Embedding, EmbeddingPurpose, EmbeddingSpace};
        Box::pin(async move {
            match (purpose, self.case) {
                (EmbeddingPurpose::Query, InvalidEmbeddingCase::QueryNonfinite) => {
                    Embedding::new(vec![1.0, f32::NAN], &self.space)
                }
                (EmbeddingPurpose::Query, InvalidEmbeddingCase::QueryWrongSpace) => {
                    let wrong = EmbeddingSpace::new("fixture", "other-model", "v1", 2)?;
                    Embedding::new(vec![1.0, 0.0], &wrong)
                }
                _ => Embedding::new(vec![1.0, 0.0], &self.space),
            }
        })
    }

    fn embed_documents(
        &self,
        texts: Vec<String>,
    ) -> neuromita::knowledge::embedding::EmbeddingFuture<
        '_,
        Vec<neuromita::knowledge::embedding::Embedding>,
    > {
        use neuromita::knowledge::embedding::{Embedding, EmbeddingSpace};
        Box::pin(async move {
            match self.case {
                InvalidEmbeddingCase::DocumentCountMismatch => Ok(Vec::new()),
                InvalidEmbeddingCase::DocumentNonfinite => {
                    Ok(vec![Embedding::new(vec![f32::INFINITY, 0.0], &self.space)?])
                }
                InvalidEmbeddingCase::DocumentWrongSpace => {
                    let wrong = EmbeddingSpace::new("fixture", "other-model", "v1", 2)?;
                    Ok(vec![Embedding::new(vec![1.0, 0.0], &wrong)?])
                }
                _ => texts
                    .into_iter()
                    .map(|_| Embedding::new(vec![1.0, 0.0], &self.space))
                    .collect(),
            }
        })
    }
}

#[tokio::test]
async fn joint_generation_rejects_malformed_query_and_document_embeddings_before_llm() {
    use neuromita::knowledge::semantic_memory::SemanticMemoryRetriever;

    let _lock = ONE_APP.lock().await;
    for case in [
        InvalidEmbeddingCase::QueryNonfinite,
        InvalidEmbeddingCase::QueryWrongSpace,
        InvalidEmbeddingCase::DocumentNonfinite,
        InvalidEmbeddingCase::DocumentWrongSpace,
        InvalidEmbeddingCase::DocumentCountMismatch,
    ] {
        let (tx, mut calls) = mpsc::unbounded_channel();
        let mut application = config(tx);
        application.knowledge = KnowledgeRetrieval::semantic(Arc::new(
            SemanticMemoryRetriever::new(Arc::new(InvalidJointEmbedding::new(case)), 32).unwrap(),
        ));
        let app = ApplicationLifecycle::start(application).await.unwrap();
        ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Archived orchid gallery".into()).unwrap(),
        )
        .unwrap();
        ChatApi::publish_message(
            CharacterId::Crazy,
            UserInput::new("Active topic".into()).unwrap(),
        )
        .unwrap();
        let previous = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
        SettingsApi::replace(
            previous.revision,
            HistoryGenerationSettings {
                mode: HistoryGenerationMode::Vector,
                active_messages: 1,
                ..previous.value
            },
        )
        .unwrap();
        let request = ChatApi::initiate(InitiateRequest::new(
            CharacterId::Crazy,
            "Orchid",
            RequestSource::Presenter,
        ))
        .unwrap();
        let done = timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                done.state,
                RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
            ),
            "{:?}",
            done.state
        );
        assert!(
            calls.try_recv().is_err(),
            "malformed query or document embedding leaked to LLM"
        );
        app.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn timeline_change_during_joint_semantic_react_rerank_cancels_before_llm() {
    use neuromita::{
        knowledge::{reranking::RerankPolicy, semantic_memory::SemanticMemoryRetriever},
        request::ReactRequest,
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let reranker = Arc::new(PausedJointReranker {
        started: Arc::new(tokio::sync::Notify::new()),
        resume: Arc::new(tokio::sync::Notify::new()),
    });
    let mut application = config(tx);
    application.knowledge = KnowledgeRetrieval::semantic(Arc::new(
        SemanticMemoryRetriever::new(Arc::new(JointEmbedding::new()), 32)
            .unwrap()
            .with_reranker(
                reranker.clone(),
                RerankPolicy {
                    alpha: 1.0,
                    limit: 4,
                },
            )
            .unwrap(),
    ));
    let app = ApplicationLifecycle::start(application).await.unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Gallery orchid archive".into()).unwrap(),
    )
    .unwrap();
    let active = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("React to this orchid".into()).unwrap(),
    )
    .unwrap();
    let previous = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        previous.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            active_messages: 1,
            ..previous.value
        },
    )
    .unwrap();
    let request = ChatApi::react(ReactRequest::new(
        CharacterId::Crazy,
        active,
        Arc::clone(UserInput::new("Gallery".into()).unwrap().text()),
        RequestSource::Presenter,
    ))
    .unwrap();
    timeout(Duration::from_secs(5), reranker.started.notified())
        .await
        .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Timeline advanced during React ranking".into()).unwrap(),
    )
    .unwrap();
    reranker.resume.notify_one();
    let done = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !matches!(
            done.state,
            RequestState::Finished(RequestOutcome::Success(_))
        ),
        "{:?}",
        done.state
    );
    assert!(
        calls.try_recv().is_err(),
        "stale React selection leaked to LLM"
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn keyword_generation_admits_active_fact_and_rag_only_but_not_hidden_or_foreign_sources() {
    use neuromita::{
        api::CharacterApi,
        memory::{MemoryCommand, MemoryId, MemoryKind, MemoryPriority, MemoryRecord},
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let fact = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Normal,
        "Saffron garden fact",
    );
    let mut rag_only = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::High,
        "Saffron garden retrieved only",
    );
    rag_only.kind = MemoryKind::RagOnly;
    let forgotten = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Saffron hidden garden",
    );
    let (fact_id, rag_only_id, forgotten_id) = (fact.id, rag_only.id, forgotten.id);
    let saved = owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![
                MemoryCommand::ImportRecord { record: fact },
                MemoryCommand::ImportRecord { record: rag_only },
                MemoryCommand::ImportRecord { record: forgotten },
            ],
        )
        .await
        .unwrap();
    owner
        .edit_memories(
            saved.snapshot,
            vec![MemoryCommand::Forget { id: forgotten_id }],
        )
        .await
        .unwrap();
    let other = CharacterApi::get(CharacterId::Kind).unwrap();
    let foreign = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Saffron foreign garden",
    );
    let foreign_id = foreign.id;
    other
        .edit_memories(
            other.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord { record: foreign }],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Saffron garden was planted last year".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Current unrelated topic".into()).unwrap(),
    )
    .unwrap();

    let off = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Saffron",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&fact_id.0.to_string()), "{evidence}");
    assert!(!evidence.contains(&rag_only_id.0.to_string()), "{evidence}");
    assert!(
        !evidence.contains(&forgotten_id.0.to_string()),
        "{evidence}"
    );
    assert!(!evidence.contains(&foreign_id.0.to_string()), "{evidence}");
    assert!(!evidence.contains("[RETRIEVED TIMELINE EVIDENCE]"));
    call.reply.send("Off response".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), off.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Keyword,
            max_results: 3,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let keyword = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Saffron",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    for id in [fact_id.0, rag_only_id.0, archived.0] {
        assert!(
            evidence.contains(&id.to_string()),
            "missing {id}: {evidence}"
        );
    }
    for id in [forgotten_id.0, foreign_id.0] {
        assert!(
            !evidence.contains(&id.to_string()),
            "leaked {id}: {evidence}"
        );
    }
    call.reply.send("Keyword response".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), keyword.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn semantic_intersect_admits_rag_only_keyword_match_without_admitting_unmatched_fact() {
    use neuromita::{
        api::CharacterApi,
        knowledge::{retrieval_fusion::FusionMode, semantic_memory::SemanticMemoryRetriever},
        memory::{MemoryCommand, MemoryId, MemoryKind, MemoryPriority, MemoryRecord},
    };

    let _lock = ONE_APP.lock().await;
    let (tx, mut calls) = mpsc::unbounded_channel();
    let mut application = config(tx);
    application.knowledge = KnowledgeRetrieval::semantic_fusion(
        Arc::new(SemanticMemoryRetriever::new(Arc::new(JointEmbedding::new()), 32).unwrap()),
        FusionMode::Intersect {
            min_methods: 2,
            require_vector: true,
        },
    )
    .unwrap();
    let app = ApplicationLifecycle::start(application).await.unwrap();
    let owner = CharacterApi::get(CharacterId::Crazy).unwrap();
    let mut rag_only = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Normal,
        "Copper orchid is in the RagOnly record",
    );
    rag_only.kind = MemoryKind::RagOnly;
    let unmatched = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Zebra amethyst waterfall",
    );
    let (rag_only_id, unmatched_id) = (rag_only.id, unmatched.id);
    owner
        .edit_memories(
            owner.memories().await.unwrap(),
            vec![
                MemoryCommand::ImportRecord { record: rag_only },
                MemoryCommand::ImportRecord { record: unmatched },
            ],
        )
        .await
        .unwrap();
    let archived = ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("The copper orchid appeared in older history".into()).unwrap(),
    )
    .unwrap();
    ChatApi::publish_message(
        CharacterId::Crazy,
        UserInput::new("Current topic is elsewhere".into()).unwrap(),
    )
    .unwrap();
    let settings = SettingsApi::snapshot::<HistoryGenerationSettings>().unwrap();
    SettingsApi::replace(
        settings.revision,
        HistoryGenerationSettings {
            mode: HistoryGenerationMode::Vector,
            max_results: 2,
            active_messages: 1,
            ..settings.value
        },
    )
    .unwrap();
    let request = ChatApi::initiate(InitiateRequest::new(
        CharacterId::Crazy,
        "Copper orchid",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = next_call(&mut calls).await;
    let evidence = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(evidence.contains(&rag_only_id.0.to_string()), "{evidence}");
    assert!(evidence.contains(&archived.0.to_string()), "{evidence}");
    assert!(
        !evidence.contains(&unmatched_id.0.to_string()),
        "{evidence}"
    );
    call.reply.send("Found authorized sources".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), request.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    app.shutdown().await.unwrap();
}
