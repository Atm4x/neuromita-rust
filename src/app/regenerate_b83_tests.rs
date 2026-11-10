use super::*;

#[cfg(feature = "sqlite-system")]
async fn b83_foreign_knowledge_change_rejects_post_dispatch_acceptance(
    change: B82ForeignKnowledgeChange,
) {
    let db = std::env::temp_dir().join(format!("b83-post-dispatch-{}.db", uuid::Uuid::new_v4()));
    let session = crate::timeline::domain::SessionId::new();
    let root = BranchId::new();
    let mut fixture = fixture(
        TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root,
        },
        2,
        true,
    )
    .await;
    let stop = tokio_util::sync::CancellationToken::new();
    let (characters, tasks) =
        crate::character::CharacterRegistry::spawn(Arc::clone(&fixture.timeline), stop.clone(), 8);
    let character = characters.get(CharacterId::Crazy).unwrap();
    let handle = fixture
        .api
        .regenerate_same_branch(fixture.source, fixture.fence)
        .unwrap();
    assert_eq!(fixture.queue.try_recv().unwrap(), handle.id());
    let record = fixture.api.registry.get(handle.id()).unwrap();
    let message = record.try_start().unwrap();
    let context = character
        .prepare(
            handle.id(),
            Arc::from(""),
            message.intent(),
            message.at(),
            record.admission(),
        )
        .await
        .unwrap();
    let gate = TimelineRequestIngress {
        timeline: Arc::clone(&fixture.timeline),
    };
    gate.before_provider(
        handle.id(),
        record.admission(),
        record.regenerate_protocol(),
    )
    .unwrap();
    assert!(matches!(
        fixture
            .api
            .inspect_orphaned_same_branch_regenerate(root, handle.id())
            .unwrap(),
        crate::timeline::RegenerateRecoveryOutcome::MayHaveDispatched(_)
    ));
    let mut foreign = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let (target, is_known) = match change {
        B82ForeignKnowledgeChange::Forget => {
            foreign
                .change_character_knowledge(
                    foreign.selection(),
                    CharacterId::Crazy,
                    EventId::new(),
                    false,
                    vec![fixture.source.original_input],
                )
                .unwrap();
            (fixture.source.original_input, false)
        }
        B82ForeignKnowledgeChange::Reveal => {
            foreign
                .change_character_knowledge(
                    foreign.selection(),
                    CharacterId::Crazy,
                    EventId::new(),
                    true,
                    vec![fixture.source.accepted_message],
                )
                .unwrap();
            (fixture.source.accepted_message, true)
        }
        B82ForeignKnowledgeChange::HistoryReset => {
            foreign
                .reset_character_context(foreign.selection(), CharacterId::Crazy, EventId::new())
                .unwrap();
            (fixture.source.original_input, false)
        }
    };
    assert_eq!(
        foreign
            .knows_event_at(CharacterId::Crazy, target, None)
            .unwrap(),
        is_known
    );
    assert!(
        foreign
            .knows_event_at(CharacterId::Crazy, target, Some(fixture.fence.at.through))
            .unwrap()
    );
    assert!(record.text_ready(Arc::from("post-dispatch response")));
    assert!(record.begin_accepting());
    let acceptance = crate::character::AcceptanceContext {
        anchor: context.anchor,
        intent: context.focus.intent,
        state_revision: context.state.revision,
        turn: context.turn,
        game: context.game_key,
        game_guard: context.game_guard,
    };
    let mut structured = crate::generation::StructuredResponse::spoken("post-dispatch response");
    structured.attitude_change = 4.0;
    let result = character
        .accept(
            handle.id(),
            acceptance,
            crate::generation::ProposedResponse::from_structured(structured).unwrap(),
            None,
            crate::timeline::conversation::ConversationAudience::private_to(CharacterId::Crazy),
        )
        .await;
    assert!(
        matches!(result, Err(crate::character::CharacterError::StaleState)),
        "stale post-dispatch acceptance must fail with StaleState: {result:?}"
    );
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    assert_eq!(
        reopened
            .character_behavior_at(CharacterId::Crazy, reopened.anchor())
            .unwrap()
            .attitude,
        context.state.attitude,
        "failed acceptance must not apply state effects"
    );
    assert!(matches!(
        reopened
            .reconcile_same_branch_regenerate(root, handle.id())
            .unwrap()
            .outcome,
        crate::timeline::RegenerateRecoveryOutcome::MayHaveDispatched(_)
    ));
    assert_eq!(
        reopened
            .knows_event_at(
                CharacterId::Crazy,
                fixture.source.original_input,
                Some(fixture.fence.at.through)
            )
            .unwrap(),
        true
    );
    assert!(matches!(
        handle.cancel(),
        crate::request::CancelResult::Cancelled | crate::request::CancelResult::Requested
    ));
    assert!(matches!(
        fixture
            .api
            .inspect_orphaned_same_branch_regenerate(root, handle.id())
            .unwrap(),
        crate::timeline::RegenerateRecoveryOutcome::Abandoned(_)
    ));
    stop.cancel();
    for task in tasks {
        task.await.unwrap();
    }
    drop((
        reopened, foreign, handle, record, message, character, characters, fixture,
    ));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_foreign_forget_after_dispatch_rejects_acceptance_without_effects() {
    b83_foreign_knowledge_change_rejects_post_dispatch_acceptance(
        B82ForeignKnowledgeChange::Forget,
    )
    .await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_foreign_reveal_after_dispatch_rejects_acceptance_without_effects() {
    b83_foreign_knowledge_change_rejects_post_dispatch_acceptance(
        B82ForeignKnowledgeChange::Reveal,
    )
    .await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_foreign_history_reset_after_dispatch_rejects_acceptance_without_effects() {
    b83_foreign_knowledge_change_rejects_post_dispatch_acceptance(
        B82ForeignKnowledgeChange::HistoryReset,
    )
    .await;
}
#[cfg(feature = "sqlite-system")]
struct B83ControlledProvider(
    mpsc::UnboundedSender<tokio::sync::oneshot::Sender<crate::generation::StructuredResponse>>,
);

#[cfg(feature = "sqlite-system")]
impl crate::generation::GenerationProvider for B83ControlledProvider {
    fn kind(&self) -> crate::generation::ProviderKind {
        crate::generation::ProviderKind::Gemini
    }

    fn generate(
        &self,
        _input: crate::generation::GenerationInput,
        _template: ProviderTemplate,
    ) -> crate::generation::ProviderFuture<'_> {
        Box::pin(async move {
            let (send, receive) = tokio::sync::oneshot::channel();
            self.0.send(send).map_err(|_| {
                crate::generation::GenerationError::Transport("test receiver closed".into())
            })?;
            receive.await.map_err(|_| {
                crate::generation::GenerationError::Transport("test response dropped".into())
            })
        })
    }
}

#[cfg(feature = "sqlite-system")]
async fn b83_run_real_message_handler_provider(change: Option<B82ForeignKnowledgeChange>) {
    let db = std::env::temp_dir().join(format!("b83-worker-{}.db", uuid::Uuid::new_v4()));
    let session = crate::timeline::domain::SessionId::new();
    let root = BranchId::new();
    let mut fixture = fixture(
        TimelineConfig::Sqlite {
            database: db.clone(),
            session,
            root,
        },
        2,
        true,
    )
    .await;
    let stop = tokio_util::sync::CancellationToken::new();
    let (characters, character_tasks) =
        crate::character::CharacterRegistry::spawn(Arc::clone(&fixture.timeline), stop.clone(), 8);
    let (provider_send, mut provider_recv) = mpsc::unbounded_channel();
    let generator =
        crate::generation::ResponseGenerator::new([Arc::new(B83ControlledProvider(provider_send))
            as Arc<dyn crate::generation::GenerationProvider>])
        .unwrap();
    let (selection_updates, context_updates) = {
        let owner = fixture.timeline.lock().unwrap();
        (
            owner.observe_selection(),
            owner.observe_context_epoch(CharacterId::Crazy),
        )
    };
    let worker = crate::request::MessageHandler {
        character: characters.get(CharacterId::Crazy).unwrap(),
        registry: Arc::clone(&fixture.api.registry),
        generator: Arc::new(generator),
        knowledge: Arc::new(crate::knowledge::KnowledgeRetrieval::lexical()),
        voice: Arc::new(crate::voice::UnavailableVoiceProvider),
        settings: Arc::clone(&fixture.settings_owner),
        presets: Arc::clone(&fixture.presets),
        prompt_preparation: crate::request::prompt_preparation::PromptPreparation::new(),
        finetune: None,
        selection_updates,
        context_updates,
        games: Arc::new(crate::games::GamePolicy::default()),
        assets: None,
        dispatch_fence: Arc::new(TimelineRequestIngress {
            timeline: Arc::clone(&fixture.timeline),
        }),
    };
    let queue = std::mem::replace(&mut fixture.queue, mpsc::channel(1).1);
    let worker_task = tokio::spawn(worker.run_queue(queue, stop.clone()));
    let handle = fixture
        .api
        .regenerate_same_branch(fixture.source, fixture.fence)
        .unwrap();
    let response_sender = tokio::time::timeout(Duration::from_secs(10), provider_recv.recv())
        .await
        .expect("provider dispatch was not entered")
        .expect("provider channel closed");
    assert!(matches!(
        fixture
            .api
            .inspect_orphaned_same_branch_regenerate(root, handle.id())
            .unwrap(),
        crate::timeline::RegenerateRecoveryOutcome::MayHaveDispatched(_)
    ));
    let mut foreign = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    if let Some(change) = change {
        match change {
            B82ForeignKnowledgeChange::Forget => foreign
                .change_character_knowledge(
                    foreign.selection(),
                    CharacterId::Crazy,
                    EventId::new(),
                    false,
                    vec![fixture.source.original_input],
                )
                .unwrap(),
            B82ForeignKnowledgeChange::Reveal => foreign
                .change_character_knowledge(
                    foreign.selection(),
                    CharacterId::Crazy,
                    EventId::new(),
                    true,
                    vec![fixture.source.accepted_message],
                )
                .unwrap(),
            B82ForeignKnowledgeChange::HistoryReset => foreign
                .reset_character_context(foreign.selection(), CharacterId::Crazy, EventId::new())
                .unwrap(),
        };
    }
    let before_state = foreign
        .character_behavior_at(CharacterId::Crazy, foreign.anchor())
        .unwrap();
    let mut result = crate::generation::StructuredResponse::spoken("controlled worker response");
    result.attitude_change = 4.0;
    response_sender
        .send(result)
        .expect("worker dropped provider response");
    let snapshot = tokio::time::timeout(Duration::from_secs(10), handle.wait())
        .await
        .expect("worker must reach a terminal outcome")
        .expect("worker resolution must be locally observable");
    let reopened = SessionTimeline::from_config(TimelineConfig::Sqlite {
        database: db.clone(),
        session,
        root,
    })
    .unwrap();
    let resulting_state = reopened
        .character_behavior_at(CharacterId::Crazy, reopened.anchor())
        .unwrap();
    match change {
        None => {
            assert!(matches!(
                snapshot.state,
                crate::request::RequestState::Finished(crate::request::RequestOutcome::Success(_))
            ));
            assert_eq!(resulting_state.attitude, before_state.attitude + 4.0);
            assert!(matches!(
                reopened
                    .reconcile_same_branch_regenerate(root, handle.id())
                    .unwrap()
                    .outcome,
                crate::timeline::RegenerateRecoveryOutcome::AcceptedReply(_)
            ));
        }
        Some(_) => {
            assert!(!matches!(
                snapshot.state,
                crate::request::RequestState::Finished(crate::request::RequestOutcome::Success(_))
            ));
            assert_eq!(resulting_state.attitude, before_state.attitude);
            assert!(matches!(
                reopened
                    .reconcile_same_branch_regenerate(root, handle.id())
                    .unwrap()
                    .outcome,
                crate::timeline::RegenerateRecoveryOutcome::Abandoned(_)
            ));
            assert!(
                reopened
                    .knows_event_at(
                        CharacterId::Crazy,
                        fixture.source.original_input,
                        Some(fixture.fence.at.through)
                    )
                    .unwrap()
            );
        }
    }
    assert!(provider_recv.try_recv().is_err());
    stop.cancel();
    worker_task.await.unwrap();
    for task in character_tasks {
        task.await.unwrap();
    }
    drop((reopened, foreign, snapshot, handle, characters, fixture));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", db.display(), suffix));
    }
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_message_handler_provider_accepts_once_with_effects() {
    b83_run_real_message_handler_provider(None).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_message_handler_provider_rejects_foreign_forget_inflight() {
    b83_run_real_message_handler_provider(Some(B82ForeignKnowledgeChange::Forget)).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_message_handler_provider_rejects_foreign_reveal_inflight() {
    b83_run_real_message_handler_provider(Some(B82ForeignKnowledgeChange::Reveal)).await;
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn same_branch_message_handler_provider_rejects_foreign_history_reset_inflight() {
    b83_run_real_message_handler_provider(Some(B82ForeignKnowledgeChange::HistoryReset)).await;
}
