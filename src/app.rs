use std::{
    collections::{BTreeMap, HashMap},
    num::NonZeroUsize,
    path::Path,
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
    ai_runtime::{
        AiService,
        application::{AiApplicationConfig, AiApplicationError, RunningAiApplication},
    },
    character::{CharacterHandle, CharacterId, CharacterRegistry, ReminderBridge},
    finetune::{FineTuneCollector, FineTuneError, FineTuneSettings},
    games::GamePolicy,
    generation::{
        EnvironmentPresetCredentials, GenerationPreset, JsonPresetRepository, PresetCatalog,
        PresetCatalogError, PresetId, PresetRepository, ResponseGenerator,
    },
    identity::EventId,
    interaction::{TurnIntent, UserInput},
    knowledge::{
        KnowledgeRetrieval,
        embedding::{EmbeddingError, EmbeddingProvider, EmbeddingSpace},
        reranking::{RerankPolicy, RerankProvider},
        retrieval_fusion::FusionMode,
        semantic_memory::SemanticMemoryRetriever,
    },
    lifecycle::Lifecycle,
    migration::{MigrationError, StartupMigrationError, StartupMigrations},
    presentation::{
        CharactersPresenter, ConnectionsPresenter, ConversationPresenter, GuiPresenter,
        PresenterRunner, PresetsPresenter, ReleasesPresenter, SettingsPresenter,
    },
    reminders::{ReminderConfig, ReminderService},
    request::{
        ChatApiState, MessageHandler, RequestId, RequestIngress, RequestRegistry, SubmitError,
    },
    runtime::{self, ApiBindings, RuntimeLease},
    server::{ServerConfig, ServerHandle, ServerLifecycle},
    settings::{
        AppSettings, JsonSettingsRepository, MemorySettingsRepository, SettingsConfig,
        SettingsError, SettingsLifecycle, SettingsSource, SettingsStartupMigration,
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

pub struct AiKnowledgeSelection {
    pub space: EmbeddingSpace,
    pub cache_entries: usize,
    pub fusion: FusionMode,
    pub reranker: Option<AiRerankSelection>,
}

pub struct AiRerankSelection {
    pub model_id: String,
    pub protocol: String,
    pub policy: RerankPolicy,
}

pub struct ApplicationConfig {
    pub releases: Arc<dyn crate::releases::ReleaseFeed>,
    pub migrations: StartupMigrations,
    pub presets: PresetCatalog,
    pub generator: ResponseGenerator,
    pub knowledge: KnowledgeRetrieval,
    pub limits: RuntimeLimits,
    pub voice: Arc<dyn VoiceProvider>,
    pub ai_runtime: Option<AiApplicationConfig>,
    pub ai_hardware: Option<std::path::PathBuf>,
    pub ai_knowledge: Option<AiKnowledgeSelection>,
    pub settings: SettingsConfig,
    pub timeline: TimelineConfig,
    pub server: ServerConfig,
    pub finetune: Option<FineTuneSettings>,
    /// Explicit gates for the character-owned mini-games.
    pub games: GamePolicy,
    pub reminders: ReminderConfig,
}

impl ApplicationConfig {
    pub fn minimal(
        preset: GenerationPreset,
        generator: ResponseGenerator,
        limits: RuntimeLimits,
        bind: std::net::SocketAddr,
    ) -> Self {
        Self::with_catalog(PresetCatalog::single(preset), generator, limits, bind)
    }

    fn with_catalog(
        presets: PresetCatalog,
        generator: ResponseGenerator,
        limits: RuntimeLimits,
        bind: std::net::SocketAddr,
    ) -> Self {
        Self {
            releases: Arc::new(crate::releases::GithubReleaseFeed::default()),
            migrations: StartupMigrations::new(),
            server: ServerConfig::test_protocol(bind, CharacterId::Crazy),
            timeline: TimelineConfig::Transient,
            presets,
            generator,
            knowledge: KnowledgeRetrieval::lexical(),
            limits,
            voice: Arc::new(UnavailableVoiceProvider),
            ai_runtime: None,
            ai_hardware: None,
            ai_knowledge: None,
            settings: SettingsConfig {
                source: SettingsSource::initial(AppSettings::initial_profile()),
                repository: Arc::new(MemorySettingsRepository::new()),
            },
            finetune: None,
            games: GamePolicy::default(),
            reminders: ReminderConfig::system(),
        }
    }

    pub fn persistent(
        preset: GenerationPreset,
        generator: ResponseGenerator,
        limits: RuntimeLimits,
        bind: std::net::SocketAddr,
        data_directory: &Path,
    ) -> Result<Self, ApplicationError> {
        Self::persistent_catalog(
            BTreeMap::from([(PresetId(0), preset)]),
            generator,
            limits,
            bind,
            data_directory,
        )
    }

    /// Starts administration/history without inventing a generation preset.
    /// Submitting generation before selecting a valid preset remains an error.
    pub fn persistent_unconfigured(
        generator: ResponseGenerator,
        limits: RuntimeLimits,
        bind: std::net::SocketAddr,
        data_directory: &Path,
    ) -> Result<Self, ApplicationError> {
        Self::persistent_catalog(BTreeMap::new(), generator, limits, bind, data_directory)
    }

    fn persistent_catalog(
        runtime: BTreeMap<PresetId, GenerationPreset>,
        generator: ResponseGenerator,
        limits: RuntimeLimits,
        bind: std::net::SocketAddr,
        data_directory: &Path,
    ) -> Result<Self, ApplicationError> {
        std::fs::create_dir_all(data_directory)?;
        let settings_path = data_directory.join("settings.json");
        let source = match std::fs::metadata(&settings_path) {
            Ok(_) => SettingsSource::Stored,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                SettingsSource::initial(AppSettings::initial_profile())
            }
            Err(error) => return Err(error.into()),
        };
        let mut config =
            Self::with_catalog(PresetCatalog::new(BTreeMap::new()), generator, limits, bind);
        config.timeline = TimelineConfig::Persistent {
            database: data_directory.join("timeline.sqlite3"),
        };
        let repository: Arc<dyn PresetRepository> = Arc::new(JsonPresetRepository::new(
            data_directory.join("presets.json"),
        ));
        config.presets =
            PresetCatalog::open(runtime, repository, Arc::new(EnvironmentPresetCredentials))?;
        config.settings = SettingsConfig {
            source,
            repository: Arc::new(JsonSettingsRepository::new(settings_path)),
        };
        let finetune_path = data_directory.join("finetune.json");
        config.finetune = match std::fs::metadata(&finetune_path) {
            Ok(_) => Some(FineTuneSettings::load(&finetune_path)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(config)
    }
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("application is starting, running or stopping")]
    Busy,
    #[error("application task failed: {0}")]
    Task(String),
    #[error(transparent)]
    MigrationPlan(#[from] MigrationError),
    #[error(transparent)]
    StartupMigration(#[from] StartupMigrationError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Presets(#[from] PresetCatalogError),
    #[error(transparent)]
    FineTune(#[from] FineTuneError),
    #[error(transparent)]
    Timeline(#[from] TimelineError),
    #[error(transparent)]
    Ai(#[from] AiApplicationError),
    #[error("AI knowledge selection requires an activated embedding worker")]
    MissingEmbeddingWorker,
    #[error("AI reranking selection requires an activated reranker worker")]
    MissingRerankerWorker,
    #[error(transparent)]
    Knowledge(#[from] EmbeddingError),
    #[error("server: {0}")]
    Server(#[from] std::io::Error),
}

pub struct ApplicationLifecycle {
    bindings: Arc<ApiBindings>,
    timeline: Arc<Mutex<SessionTimeline>>,
    presenter: GuiPresenter,
    server: ServerHandle,
    finetune: Option<FineTuneCollector>,
    speech: Option<Arc<crate::ai_runtime::speech::PythonSpeechRecognizer>>,
    id: Uuid,
    stop: CancellationToken,
    completion: Option<oneshot::Receiver<Result<(), ApplicationError>>>,
}

struct TimelineRequestIngress {
    timeline: Arc<Mutex<SessionTimeline>>,
}

impl RequestIngress for TimelineRequestIngress {
    fn publish_conversation_message(
        &self,
        message: ConversationMessage,
    ) -> Result<EventId, SubmitError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        let selection = timeline.selection();
        let event_id = message.id;
        timeline
            .publish_conversation_message(selection, message)
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        Ok(event_id)
    }

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
        character: CharacterId,
        intent: TurnIntent,
    ) -> Result<TimelineAdmission, SubmitError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        let selection = timeline.selection();
        timeline
            .assert_selection(selection)
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        if let TurnIntent::Reply(_) | TurnIntent::React(_) = intent {
            timeline
                .reaction_cause_at(character, intent, timeline.anchor())
                .map_err(|_| SubmitError::InvalidTargetEvent)?;
        }
        timeline
            .admission(character, selection)
            .map_err(|_| SubmitError::TimelineUnavailable)
    }
}

struct ApplicationOwner {
    finetune: Option<FineTuneCollector>,
    releases_task: Option<JoinHandle<Result<(), std::convert::Infallible>>>,
    bindings: Arc<ApiBindings>,
    reminders_stop: CancellationToken,
    reminders_task: Option<JoinHandle<()>>,
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
    ai_runtime: Option<RunningAiApplication>,
}

impl ApplicationLifecycle {
    pub async fn start(config: ApplicationConfig) -> Result<Self, ApplicationError> {
        let lease = RuntimeLease::reserve().ok_or(ApplicationError::Busy)?;
        let id = lease.id();
        let ApplicationConfig {
            releases: release_feed,
            migrations,
            presets,
            generator,
            knowledge,
            limits,
            voice,
            ai_runtime,
            ai_hardware,
            ai_knowledge,
            settings,
            timeline,
            server,
            finetune,
            games,
            reminders: reminders_config,
        } = config;
        let finetune = finetune
            .map(|settings| FineTuneCollector::from_settings(&settings))
            .transpose()?;
        let mut startup = StartupMigrations::new();
        if matches!(&settings.source, SettingsSource::Stored) {
            startup.register(SettingsStartupMigration(Arc::clone(&settings.repository)))?;
        }
        startup.append(migrations)?;
        let (lease, migrated) = tokio::task::spawn_blocking(move || {
            let result = startup.run();
            (lease, result)
        })
        .await
        .map_err(|error| ApplicationError::Task(error.to_string()))?;
        migrated?;
        let settings = SettingsLifecycle::prepare(settings).await?;
        let settings_owner = settings.owner();
        let (server_lifecycle, server) = ServerLifecycle::bind(server).await?;
        let timeline = Arc::new(Mutex::new(SessionTimeline::from_config(timeline)?));
        let ai_runtime = match (ai_runtime, ai_hardware) {
            (Some(configuration), Some(path)) => {
                let store = crate::ai_runtime::hardware::HardwareConfigStore::new(path);
                let selected = store.load().map_err(|error| {
                    crate::ai_runtime::application::AiApplicationError::Invalid(error.to_string())
                })?;
                Some(Box::pin(configuration.activate_with_hardware(&selected, &store)).await?)
            }
            (Some(configuration), None) => Some(Box::pin(configuration.activate()).await?),
            (None, Some(_)) => {
                return Err(crate::ai_runtime::application::AiApplicationError::Invalid(
                    "hardware selection requires an explicit AI application plan".into(),
                )
                .into());
            }
            (None, None) => None,
        };
        let speech = ai_runtime
            .as_ref()
            .and_then(|runtime| runtime.speech_recognizer())
            .map(Arc::new);
        let voice = match &ai_runtime {
            Some(runtime) if runtime.has_service(AiService::Tts) => runtime.voice_provider(),
            _ => voice,
        };
        let knowledge = match ai_knowledge {
            None => knowledge,
            Some(selection) => {
                let runtime = ai_runtime
                    .as_ref()
                    .ok_or(ApplicationError::MissingEmbeddingWorker)?;
                let adapter = runtime
                    .embedding_provider(selection.space)
                    .ok_or(ApplicationError::MissingEmbeddingWorker)?;
                let provider: Arc<dyn EmbeddingProvider> = Arc::new(adapter);
                let mut retriever =
                    SemanticMemoryRetriever::new(provider, selection.cache_entries)?;
                if let Some(reranker) = selection.reranker {
                    let adapter = runtime
                        .reranker(reranker.model_id, reranker.protocol)
                        .ok_or(ApplicationError::MissingRerankerWorker)?
                        .map_err(|error| EmbeddingError::Configuration(error.to_string()))?;
                    let scorer: Arc<dyn RerankProvider> = Arc::new(adapter);
                    retriever = retriever.with_reranker(scorer, reranker.policy)?;
                }
                KnowledgeRetrieval::semantic_fusion(Arc::new(retriever), selection.fusion)?
            }
        };
        let registry = Arc::new(RequestRegistry::new_with_diagnostics(
            limits.discovery_buffer.get(),
            crate::request::DiagnosticsLimits::default(),
        ));
        let selection_updates = timeline
            .lock()
            .expect("timeline lock poisoned")
            .observe_selection();
        let workers_stop = CancellationToken::new();
        let owners_stop = CancellationToken::new();
        let presenter_stop = CancellationToken::new();
        let (releases, release_lifecycle) = ReleasesPresenter::new(release_feed);
        let releases_task = tokio::spawn(release_lifecycle.run(presenter_stop.child_token()));
        let reminders_wake = Arc::new(tokio::sync::Notify::new());
        let (characters, owners) = CharacterRegistry::spawn_with_reminders(
            Arc::clone(&timeline),
            owners_stop.clone(),
            limits.character_commands.get(),
            ReminderBridge {
                settings: Arc::clone(&settings_owner),
                wake: Arc::clone(&reminders_wake),
                clock: Arc::clone(&reminders_config.clock),
            },
        );
        let generator = Arc::new(generator);
        let knowledge = Arc::new(knowledge);
        let presets = Arc::new(presets);
        let games = Arc::new(games);
        for id in CharacterId::ALL {
            characters
                .get(id)
                .expect("hardcoded character exists")
                .configure_game_policy(*games)
                .await
                .map_err(|error| ApplicationError::Task(error.to_string()))?;
        }
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
                knowledge: Arc::clone(&knowledge),
                voice: Arc::clone(&voice),
                settings: Arc::clone(&settings_owner),
                presets: Arc::clone(&presets),
                finetune: finetune.clone(),
                selection_updates: selection_updates.clone(),
                context_updates,
                games: Arc::clone(&games),
            };
            workers.push(tokio::spawn(
                worker.run_queue(receive, workers_stop.clone()),
            ));
        }
        let ingress: Arc<dyn RequestIngress> = Arc::new(TimelineRequestIngress {
            timeline: Arc::clone(&timeline),
        });
        let api = Arc::new(ChatApiState::new(registry, handlers, ingress));
        let reminders_stop = CancellationToken::new();
        let (reminders, reminders_task) = ReminderService::start(
            Arc::clone(&timeline),
            Arc::clone(&api),
            Arc::clone(&settings_owner),
            reminders_config,
            reminders_wake,
            reminders_stop.clone(),
        );
        let discovery = api.registry.observe();
        let (chat, runner) = PresenterRunner::new(
            Arc::clone(&api),
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
            presets: Arc::clone(&presets),
            games: Arc::clone(&games),
            reminders,
        });
        let mut owner = ApplicationOwner {
            finetune: finetune.clone(),
            releases_task: Some(releases_task),
            bindings: Arc::clone(&bindings),
            reminders_stop,
            reminders_task: Some(reminders_task),
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
            ai_runtime,
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
                releases,
                connections: ConnectionsPresenter::new(server.clone()),
                conversation: ConversationPresenter::new(
                    chat.clone(),
                    CharactersPresenter::new(bindings.characters.clone()),
                    Arc::clone(&timeline),
                    Arc::clone(&bindings.settings),
                    Arc::clone(&presets),
                ),
                chat,
                settings: settings_presenter,
                presets: PresetsPresenter::new(Arc::clone(&presets)),
                characters: CharactersPresenter::new(bindings.characters.clone()),
            },
            bindings,
            timeline,
            server,
            finetune,
            speech,
            id,
            stop,
            completion: Some(completion),
        })
    }

    /// An ASR-powered microphone session is available only when ASR was explicitly activated.
    /// Its cancellation token is a child of the application's lifecycle token.
    pub fn speech_session(
        &self,
        character: CharacterId,
        source: crate::request::RequestSource,
        playback: Arc<dyn crate::audio::device::AudioPlayback>,
    ) -> Option<(crate::audio::session::SpeechInputHandler, CancellationToken)> {
        let recognizer = self.speech.as_ref()?;
        let handler = crate::audio::session::SpeechInputHandler::new(
            Arc::clone(recognizer) as Arc<dyn crate::audio::session::SpeechTranscriber>,
            Arc::new(crate::audio::session::ChatReplyTarget { character, source }),
            playback,
        );
        Some((handler, self.stop.child_token()))
    }

    pub fn presenter(&self) -> &GuiPresenter {
        &self.presenter
    }
    pub fn server(&self) -> &ServerHandle {
        &self.server
    }
    pub fn finetune(&self) -> Option<&FineTuneCollector> {
        self.finetune.as_ref()
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
        // Stop the reminder scheduler before character workers so an in-flight
        // delivery is interrupted with the Shutdown cause and stays pending.
        self.reminders_stop.cancel();
        if let Some(task) = self.reminders_task.take()
            && let Err(error) = task.await
        {
            failure = Some(ApplicationError::Task(error.to_string()));
        }
        self.workers_stop.cancel();
        for worker in self.workers.drain(..) {
            if let Err(error) = worker.await {
                failure = Some(ApplicationError::Task(error.to_string()));
            }
        }
        if let Some(collector) = &self.finetune {
            let status = collector.drain(crate::finetune::DELIVERY_DRAIN, true).await;
            if status.pending > 0 || status.errors > 0 || status.dropped > 0 {
                tracing::warn!(
                    pending = status.pending,
                    errors = status.errors,
                    dropped = status.dropped,
                    "dataset delivery stopped with incomplete records"
                );
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
        if let Some(task) = self.releases_task.take()
            && let Err(error) = task.await
        {
            failure = Some(ApplicationError::Task(error.to_string()));
        }
        if let Some(task) = self.presenter_task.take()
            && let Err(error) = task.await
        {
            failure = Some(ApplicationError::Task(error.to_string()));
        }
        if let Some(runtime) = self.ai_runtime.take()
            && let Err(error) = runtime.shutdown().await
        {
            failure = Some(ApplicationError::Ai(error));
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
        if let Some(collector) = &self.finetune {
            collector.stop_delivery();
        }
        self.bindings.chat.close();
        self.bindings.chat.registry.close();
        self.workers_stop.cancel();
        self.presenter_stop.cancel();
        if let Some(task) = &self.releases_task {
            task.abort();
        }
        self.server_stop.cancel();
        self.reminders_stop.cancel();
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
        if let Some(task) = &self.reminders_task {
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
