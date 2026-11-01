use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use crate::runtime::ApplicationStatus;

use crate::{
    character::{CharacterHandle, CharacterId, CharacterRegistry},
    generation::{GenerationPreset, ResponseGenerator},
    identity::EventId,
    interaction::{TurnIntent, UserInput},
    lifecycle::Lifecycle,
    presentation::{CharactersPresenter, GuiPresenter, PresenterRunner, SettingsPresenter},
    request::{
        ChatApiState, MessageHandler, RequestId, RequestIngress, RequestRegistry, SubmitError,
    },
    runtime::{self, ApiBindings, RuntimeLease},
    server::{ServerConfig, ServerHandle, ServerLifecycle},
    settings::{
        AppSettings, MemorySettingsRepository, SettingsConfig, SettingsError, SettingsLifecycle,
        SettingsSource,
    },
    timeline::{
        SessionTimeline, TimelineConfig, TimelineEvent,
        conversation::ConversationMessage,
        domain::{BranchId, TimelineError, TimelineFact},
        ingress::SemanticBatch,
        message_actions::MessageAction,
        selection::{TimelineAdmission, TimelineSelection},
    },
    voice::{UnavailableVoiceProvider, VoiceProvider},
};

pub struct RuntimeLimits {
    pub request_queue: NonZeroUsize,
    pub discovery_buffer: NonZeroUsize,
    pub character_commands: NonZeroUsize,
    pub presenter_updates: NonZeroUsize,
    pub retained_finished: NonZeroUsize,
}

pub struct ApplicationConfig {
    pub preset: GenerationPreset,
    pub generator: ResponseGenerator,
    pub limits: RuntimeLimits,
    pub voice: Arc<dyn VoiceProvider>,
    pub settings: SettingsConfig,
    pub timeline: TimelineConfig,
    pub server: ServerConfig,
}

impl ApplicationConfig {
    pub fn minimal(
        preset: GenerationPreset,
        generator: ResponseGenerator,
        limits: RuntimeLimits,
        bind: std::net::SocketAddr,
    ) -> Self {
        Self {
            server: ServerConfig::test_protocol(bind, CharacterId::Mita, preset.clone()),
            timeline: TimelineConfig::Transient,
            preset,
            generator,
            limits,
            voice: Arc::new(UnavailableVoiceProvider),
            settings: SettingsConfig {
                source: SettingsSource::Initial(AppSettings::initial_profile()),
                repository: Arc::new(MemorySettingsRepository::new()),
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("application is starting, running or stopping")]
    Busy,
    #[error("application task failed: {0}")]
    Task(String),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Timeline(#[from] TimelineError),
    #[error("server: {0}")]
    Server(#[from] std::io::Error),
}

pub struct ApplicationLifecycle {
    bindings: Arc<ApiBindings>,
    timeline: Arc<Mutex<SessionTimeline>>,
    presenter: GuiPresenter,
    server: ServerHandle,
    id: Uuid,
    stop: CancellationToken,
    completion: Option<oneshot::Receiver<Result<(), ApplicationError>>>,
}

struct TimelineRequestIngress {
    timeline: Arc<Mutex<SessionTimeline>>,
}

impl RequestIngress for TimelineRequestIngress {
    fn publish_message(
        &self,
        character: CharacterId,
        input: &UserInput,
    ) -> Result<EventId, SubmitError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        timeline
            .assert_selection(timeline.selection())
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        timeline
            .publish_player_message(character, input.event_id(), Arc::clone(input.text()))
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        Ok(input.event_id())
    }

    fn admit(
        &self,
        request: RequestId,
        character: CharacterId,
        input: &UserInput,
        intent: TurnIntent,
    ) -> Result<TimelineAdmission, SubmitError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        let selection = timeline.selection();
        timeline
            .assert_selection(selection)
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        if intent == TurnIntent::Reply {
            timeline
                .append_player_message(
                    character,
                    request,
                    input.event_id(),
                    Arc::clone(input.text()),
                )
                .map_err(|_| SubmitError::TimelineUnavailable)?;
        }
        timeline
            .admission(character, selection)
            .map_err(|_| SubmitError::TimelineUnavailable)
    }
}

struct ApplicationOwner {
    bindings: Arc<ApiBindings>,
    workers_stop: CancellationToken,
    owners_stop: CancellationToken,
    presenter_stop: CancellationToken,
    workers: Vec<JoinHandle<()>>,
    owners: Vec<JoinHandle<()>>,
    presenter_task: Option<JoinHandle<()>>,
    settings_stop: CancellationToken,
    settings_task: Option<JoinHandle<Result<(), SettingsError>>>,
    server_stop: CancellationToken,
    server_task: Option<JoinHandle<std::io::Result<()>>>,
    lease: RuntimeLease,
}

impl ApplicationLifecycle {
    pub async fn start(config: ApplicationConfig) -> Result<Self, ApplicationError> {
        let lease = RuntimeLease::reserve().ok_or(ApplicationError::Busy)?;
        let id = lease.id();
        let ApplicationConfig {
            preset,
            generator,
            limits,
            voice,
            settings,
            timeline,
            server,
        } = config;
        let settings = SettingsLifecycle::prepare(settings).await?;
        let (server_lifecycle, server) = ServerLifecycle::bind(server).await?;
        let registry = Arc::new(RequestRegistry::new(limits.discovery_buffer.get()));
        let timeline = Arc::new(Mutex::new(SessionTimeline::from_config(timeline)?));
        let selection_updates = timeline
            .lock()
            .expect("timeline lock poisoned")
            .observe_selection();
        let workers_stop = CancellationToken::new();
        let owners_stop = CancellationToken::new();
        let presenter_stop = CancellationToken::new();
        let (characters, owners) = CharacterRegistry::spawn(
            Arc::clone(&timeline),
            owners_stop.clone(),
            limits.character_commands.get(),
        );
        let generator = Arc::new(generator);
        let mut handlers = HashMap::new();
        let mut workers = Vec::new();
        for id in CharacterId::ALL {
            let (send, receive) = mpsc::channel(limits.request_queue.get());
            handlers.insert(id, send);
            let context_updates = timeline
                .lock()
                .expect("timeline lock poisoned")
                .observe_context_epoch(id);
            let worker = MessageHandler {
                character: characters.get(id).expect("hardcoded character exists"),
                registry: Arc::clone(&registry),
                generator: Arc::clone(&generator),
                voice: Arc::clone(&voice),
                selection_updates: selection_updates.clone(),
                context_updates,
            };
            workers.push(tokio::spawn(
                worker.run_queue(receive, workers_stop.clone()),
            ));
        }
        let ingress: Arc<dyn RequestIngress> = Arc::new(TimelineRequestIngress {
            timeline: Arc::clone(&timeline),
        });
        let api = Arc::new(ChatApiState::new(registry, handlers, ingress));
        let discovery = api.registry.observe();
        let settings_owner = settings.owner();
        let (chat, runner) = PresenterRunner::new(
            Arc::clone(&api),
            Arc::clone(&settings_owner),
            preset,
            discovery,
            limits.presenter_updates.get(),
            limits.retained_finished.get(),
        );
        let presenter_task = Some(tokio::spawn(runner.run(presenter_stop.clone())));
        let settings_presenter = SettingsPresenter::new(Arc::clone(&settings_owner));
        let settings_stop = CancellationToken::new();
        let settings_task = Some(tokio::spawn(settings.run(settings_stop.clone())));
        let server_stop = CancellationToken::new();
        let bindings = Arc::new(ApiBindings {
            chat: api,
            characters,
            settings: settings_owner,
        });
        let mut owner = ApplicationOwner {
            bindings: Arc::clone(&bindings),
            workers_stop,
            owners_stop,
            presenter_stop,
            workers,
            owners,
            presenter_task,
            settings_stop,
            settings_task,
            server_stop,
            server_task: None,
            lease,
        };
        owner.lease.publish(Arc::clone(&bindings));
        owner.server_task = Some(tokio::spawn(
            server_lifecycle.run_with_api(owner.server_stop.clone(), Arc::clone(&bindings)),
        ));
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let (completed, completion) = oneshot::channel();
        tokio::spawn(async move {
            stopped.cancelled().await;
            let result = owner.shutdown().await;
            let _ = completed.send(result);
        });
        Ok(Self {
            presenter: GuiPresenter {
                chat,
                settings: settings_presenter,
                characters: CharactersPresenter::new(bindings.characters.clone()),
            },
            bindings,
            timeline,
            server,
            id,
            stop,
            completion: Some(completion),
        })
    }

    pub fn presenter(&self) -> &GuiPresenter {
        &self.presenter
    }
    pub fn server(&self) -> &ServerHandle {
        &self.server
    }
    pub fn at(&self) -> crate::timeline::ContextAnchor {
        self.timeline
            .lock()
            .expect("timeline lock poisoned")
            .anchor()
    }
    pub fn at_event(
        &self,
        event: crate::timeline::EventId,
    ) -> Option<crate::timeline::ContextAnchor> {
        self.timeline
            .lock()
            .expect("timeline lock poisoned")
            .at_event(event)
    }
    pub fn character(&self, id: CharacterId) -> Option<CharacterHandle> {
        self.bindings.characters.get(id)
    }
    pub fn timeline_selection(&self) -> TimelineSelection {
        self.timeline
            .lock()
            .expect("timeline lock poisoned")
            .selection()
    }

    pub fn select_timeline_branch(
        &self,
        branch: BranchId,
        expected: TimelineSelection,
    ) -> Result<TimelineSelection, TimelineError> {
        self.timeline
            .lock()
            .expect("timeline lock poisoned")
            .select_branch(branch, expected)
    }

    /// Publish an explicitly identified group of semantic world events.
    /// This does not enqueue a model request or interpret Unity wire messages.
    pub fn publish_semantic_events(
        &self,
        expected: TimelineSelection,
        batch: SemanticBatch,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .ingest_semantic_events(expected, batch)
    }

    /// Publish one conversation fact shared by its witnesses, without generating a reply.
    pub fn publish_conversation_message(
        &self,
        expected: TimelineSelection,
        message: ConversationMessage,
    ) -> Result<TimelineFact, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .publish_conversation_message(expected, message)
    }

    /// Make a committed conversation message known to additional listeners.
    pub fn disclose_conversation_message(
        &self,
        expected: TimelineSelection,
        operation: EventId,
        source: EventId,
        witnesses: std::collections::BTreeSet<CharacterId>,
    ) -> Result<TimelineFact, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .disclose_conversation_message(expected, operation, source, witnesses)
    }

    /// Change the presentation of already known conversation messages for one character.
    /// The operation EventId must be preserved when retrying an uncertain write.
    pub fn change_message(
        &self,
        expected: TimelineSelection,
        character: CharacterId,
        operation: EventId,
        action: MessageAction,
    ) -> Result<TimelineFact, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .change_message(expected, character, operation, action)
    }

    /// Forget current knowledge of one character without deleting canonical events.
    /// In-flight local generation for that character is invalidated after commit.
    pub fn reset_character_context(
        &self,
        expected: TimelineSelection,
        character: CharacterId,
        id: EventId,
    ) -> Result<EventId, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .reset_character_context(expected, character, id)
    }

    /// Read-only reconciliation of a request after an uncertain shutdown.
    /// Some proves the reply was committed in the selected historical prefix;
    /// None does not prove that the request was cancelled or failed.
    /// A successful lookup never replays generation or effects.
    pub fn accepted_reply_at(
        &self,
        character: CharacterId,
        request: RequestId,
        at: crate::timeline::ContextAnchor,
    ) -> Result<Option<TimelineFact>, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .accepted_reply_at(character, request, at)
    }

    /// Inspect the strongest durable evidence of a request in one historical prefix.
    /// It never recreates a RequestHandle, repeats generation or infers cancellation.
    pub fn request_evidence_at(
        &self,
        character: CharacterId,
        request: RequestId,
        at: crate::timeline::ContextAnchor,
    ) -> Result<crate::timeline::domain::RequestEvidence, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .request_evidence_at(character, request, at)
    }

    /// Reconcile by the input EventId retained before submitting a request.
    /// NoRecord and an absent match are not evidence of cancellation or failure.
    pub fn request_evidence_for_input_at(
        &self,
        character: CharacterId,
        input: EventId,
        at: crate::timeline::ContextAnchor,
    ) -> Result<Option<(RequestId, crate::timeline::domain::RequestEvidence)>, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?
            .request_evidence_for_input_at(character, input, at)
    }

    pub fn timeline(&self) -> Vec<TimelineEvent> {
        self.timeline
            .lock()
            .expect("timeline lock poisoned")
            .events()
    }
    pub fn active_requests(&self) -> usize {
        self.bindings.chat.registry.active_count()
    }

    pub fn status() -> ApplicationStatus {
        runtime::status()
    }

    pub async fn wait_stopped() {
        runtime::wait_stopped().await;
    }

    pub async fn restart(self, config: ApplicationConfig) -> Result<Self, ApplicationError> {
        self.shutdown().await?;
        Self::start(config).await
    }

    pub async fn shutdown(mut self) -> Result<(), ApplicationError> {
        self.request_stop();
        self.completion
            .take()
            .expect("application owns its completion")
            .await
            .map_err(|error| ApplicationError::Task(error.to_string()))?
    }

    fn request_stop(&self) {
        if !self.stop.is_cancelled() {
            self.bindings.chat.close();
            self.bindings.settings.close();
            runtime::begin_stopping(self.id);
            self.stop.cancel();
        }
    }
}

impl ApplicationOwner {
    async fn shutdown(mut self) -> Result<(), ApplicationError> {
        let mut failure = None;
        self.server_stop.cancel();
        if let Some(task) = self.server_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failure = Some(ApplicationError::Server(error)),
                Err(error) => failure = Some(ApplicationError::Task(error.to_string())),
            }
        }
        self.workers_stop.cancel();
        for worker in self.workers.drain(..) {
            if let Err(error) = worker.await {
                failure = Some(ApplicationError::Task(error.to_string()));
            }
        }
        self.bindings.chat.registry.close();
        self.owners_stop.cancel();
        for owner in self.owners.drain(..) {
            if let Err(error) = owner.await {
                failure = Some(ApplicationError::Task(error.to_string()));
            }
        }
        self.presenter_stop.cancel();
        if let Some(task) = self.presenter_task.take()
            && let Err(error) = task.await
        {
            failure = Some(ApplicationError::Task(error.to_string()));
        }
        self.settings_stop.cancel();
        if let Some(task) = self.settings_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failure = Some(ApplicationError::Settings(error)),
                Err(error) => failure = Some(ApplicationError::Task(error.to_string())),
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for ApplicationLifecycle {
    fn drop(&mut self) {
        self.request_stop();
    }
}

impl Drop for ApplicationOwner {
    fn drop(&mut self) {
        self.bindings.chat.close();
        self.bindings.chat.registry.close();
        self.workers_stop.cancel();
        self.presenter_stop.cancel();
        self.server_stop.cancel();
        self.bindings.settings.close();
        self.settings_stop.cancel();
        self.owners_stop.cancel();
        for task in self
            .workers
            .iter()
            .chain(self.owners.iter())
            .chain(self.presenter_task.iter())
        {
            task.abort();
        }
        if let Some(task) = &self.server_task {
            task.abort();
        }
        if let Some(task) = &self.settings_task {
            task.abort();
        }
    }
}

impl Lifecycle for ApplicationLifecycle {
    type Error = ApplicationError;
    async fn run(self, stop: CancellationToken) -> Result<(), Self::Error> {
        stop.cancelled().await;
        self.shutdown().await
    }
}
