pub mod historical_recall;

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
        install_coordinator::AiHubInstallCoordinator,
        live_speech::{
            LiveSpeechRuntime, LiveSpeechTranscriber, LiveSpeechVoice, watch_speech_settings,
        },
        model_identity::{AiModelIdentity, WorkerImplementation},
        native_service::AiHubNativeService,
        native_speech::{RoutedSpeechTranscriber, RoutedSpeechVoice},
        semantic_workers::{ServiceEmbeddingProvider, ServiceRerankProvider},
        service_workers::{ServiceModel, ServiceWorkerRegistry, WorkerClaim},
    },
    character::{CharacterHandle, CharacterId, CharacterRegistry, ReminderBridge},
    finetune::{FineTuneCollector, FineTuneError, FineTuneSettings},
    game_link::{GameBeatConfig, GameLinkConfig, GameLinkHandle, GameLinkLifecycle},
    games::GamePolicy,
    generation::{
        GenerationPreset, JsonPresetRepository, PresetCatalog, PresetCatalogError, PresetId,
        PresetRepository, ResponseGenerator,
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
    prompts::PromptCatalog,
    reminders::{ReminderConfig, ReminderService},
    request::{
        ChatApiState, MessageHandler, ProviderDispatchFence, RegenerateCancellation,
        RegenerateProtocol, RequestId, RequestIngress, RequestRegistry, SubmitError,
    },
    runtime::{self, ApiBindings, RuntimeLease},
    server::{ServerConfig, ServerHandle, ServerLifecycle},
    settings::{
        AppSettings, LeasedJsonSettingsRepository, MemorySettingsRepository, SettingsConfig,
        SettingsError, SettingsInstanceLease, SettingsLifecycle, SettingsSource,
        SettingsStartupMigration,
    },
    timeline::{
        RegenerateConfigurationReference, RegenerateRecoveryOutcome, RegenerateSource,
        SameBranchRegenerateReceipt, SessionTimeline, TimelineConfig, TimelineEvent,
        conversation::ConversationMessage,
        domain::{BranchId, TimelineError, TimelineFact},
        ingress::SemanticBatch,
        message_actions::MessageAction,
        selection::{TimelineAdmission, TimelineSelection},
    },
    voice::{UnavailableVoiceProvider, VoiceProvider},
};

// Infrastructure lease ownership follows the persistent repository lifetime;
// the generation domain never needs to know about lock files.
struct LeasedPresetRepository {
    inner: JsonPresetRepository,
    _lease: crate::settings::PresetStorageLease,
}

impl PresetRepository for LeasedPresetRepository {
    fn load(&self) -> Result<Option<crate::generation::StoredPresets>, PresetCatalogError> {
        self.inner.load()
    }

    fn save(&self, value: &crate::generation::StoredPresets) -> Result<(), PresetCatalogError> {
        self.inner.save(value)
    }
}

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
    pub prompts: Option<Arc<PromptCatalog>>,
    pub generator: ResponseGenerator,
    pub knowledge: KnowledgeRetrieval,
    pub graph_extractor: Option<Arc<dyn crate::knowledge::graph::GraphExtractor>>,
    pub limits: RuntimeLimits,
    pub voice: Arc<dyn VoiceProvider>,
    pub ai_runtime: Option<AiApplicationConfig>,
    pub ai_hub_install_root: Option<std::path::PathBuf>,
    pub ai_speech_inventory: Option<crate::ai_runtime::speech_catalog::SpeechInventory>,
    pub ai_installed_speech:
        Option<crate::ai_runtime::installed_speech::InstalledSpeechEnvironment>,
    pub ai_hardware: Option<std::path::PathBuf>,
    pub ai_knowledge: Option<AiKnowledgeSelection>,
    pub settings: SettingsConfig,
    settings_instance: Option<Arc<SettingsInstanceLease>>,
    pub timeline: TimelineConfig,
    pub timeline_assets: Option<Arc<crate::timeline::storage::assets::TimelineAssetStore>>,
    pub server: ServerConfig,
    pub game_link: Option<GameLinkConfig>,
    /// Optional, explicitly scoped game Beat Sync; disabled by default.
    pub game_beats: Option<GameBeatConfig>,
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
            server: ServerConfig::local_control(bind),
            game_link: None,
            game_beats: None,
            timeline: TimelineConfig::Transient,
            timeline_assets: None,
            presets,
            prompts: None,
            generator,
            knowledge: KnowledgeRetrieval::lexical(),
            graph_extractor: None,
            limits,
            voice: Arc::new(UnavailableVoiceProvider),
            ai_runtime: None,
            ai_hub_install_root: None,
            ai_speech_inventory: None,
            ai_installed_speech: None,
            ai_hardware: None,
            ai_knowledge: None,
            settings: SettingsConfig {
                source: SettingsSource::initial(AppSettings::initial_profile()),
                repository: Arc::new(MemorySettingsRepository::new()),
            },
            settings_instance: None,
            finetune: None,
            games: GamePolicy::default(),
            reminders: ReminderConfig::system(),
        }
    }

    /// Supply an explicitly selected, typed graph extraction backend.
    /// Without one, Graph Reindex fails closed instead of synthesizing relations.
    pub fn with_graph_extractor(
        mut self,
        extractor: Arc<dyn crate::knowledge::graph::GraphExtractor>,
    ) -> Self {
        self.graph_extractor = Some(extractor);
        self
    }

    pub fn with_prompt_catalog(mut self, catalog: PromptCatalog) -> Self {
        self.prompts = Some(Arc::new(catalog));
        self
    }

    /// Enable Core-owned installation admission for an explicitly configured data root.
    pub fn with_ai_hub_install_root(mut self, root: std::path::PathBuf) -> Self {
        self.ai_hub_install_root = Some(root);
        self
    }

    /// Attach an already prepared AI plan and an authoritative read-only speech catalog.
    /// Persisted TTS/ASR settings are validated before any worker starts.
    /// This does not prepare or install models.
    pub fn with_prepared_speech_runtime(
        mut self,
        plan: AiApplicationConfig,
        inventory: crate::ai_runtime::speech_catalog::SpeechInventory,
    ) -> Self {
        self.ai_runtime = Some(plan);
        self.ai_speech_inventory = Some(inventory);
        self
    }

    /// Resolve already installed speech workers after settings migration.
    /// Missing dependencies never cause installs, downloads or backend substitution.
    pub fn with_installed_speech_runtime(
        mut self,
        environment: crate::ai_runtime::installed_speech::InstalledSpeechEnvironment,
    ) -> Self {
        self.ai_installed_speech = Some(environment);
        self
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
        let instance = Arc::new(
            SettingsInstanceLease::acquire(data_directory)
                .map_err(|error| SettingsError::Storage(error.to_string()))?,
        );
        let preset_lease = crate::settings::PresetActivation::runtime_lease(data_directory)?;
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
        config.timeline_assets = Some(Arc::new(
            crate::timeline::storage::assets::TimelineAssetStore::new(
                data_directory.join("TimelineAssets"),
                8 * 1024 * 1024,
            )?,
        ));
        let repository: Arc<dyn PresetRepository> = Arc::new(LeasedPresetRepository {
            inner: JsonPresetRepository::new(data_directory.join("presets.json")),
            _lease: preset_lease,
        });
        config.presets = PresetCatalog::open(
            runtime,
            repository,
            Arc::new(crate::generation::ApplicationPresetCredentials::new(
                data_directory.join("preset-credentials.json"),
            )),
        )?;
        config.settings = SettingsConfig {
            source,
            repository: Arc::new(LeasedJsonSettingsRepository::new(
                settings_path,
                Arc::clone(&instance),
            )),
        };
        config.settings_instance = Some(instance);
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
    PresetActivation(#[from] crate::settings::PresetActivationError),
    #[error(transparent)]
    FineTune(#[from] FineTuneError),
    #[error(transparent)]
    Timeline(#[from] TimelineError),
    #[error(transparent)]
    Assets(#[from] crate::timeline::storage::assets::TimelineAssetError),
    #[error(transparent)]
    Ai(#[from] AiApplicationError),
    #[error(transparent)]
    SpeechBinding(#[from] crate::ai_runtime::application::AiSpeechBindingError),
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
    game_link: Option<GameLinkHandle>,
    finetune: Option<FineTuneCollector>,
    speech: Option<Arc<dyn crate::audio::session::SpeechTranscriber>>,
    audio_input: Arc<crate::audio::input_owner::AudioInputOwner>,
    speech_controller: Arc<tokio::sync::Mutex<crate::audio::controller::SpeechSessionController>>,
    speech_startup_warning: Option<String>,
    ai_recovery: Option<crate::ai_runtime::application::AiRecoveryHandle>,
    recovery_workers: Arc<ServiceWorkerRegistry>,
    ai_hub_install: Option<Arc<AiHubInstallCoordinator>>,
    ai_hub_native: Option<Arc<AiHubNativeService>>,
    id: Uuid,
    stop: CancellationToken,
    completion: Option<oneshot::Receiver<Result<(), ApplicationError>>>,
}

/// The top-level owner samples Timeline after local notifications or an
/// occasional idle poll. Notifications only schedule a read; a foreign SQLite
/// writer never has to deliver them. No provider request is made here.
fn reconcile_semantic_knowledge(
    timeline: &Mutex<SessionTimeline>,
    knowledge: &KnowledgeRetrieval,
    character: CharacterId,
) -> Result<(), String> {
    let timeline = timeline.lock().map_err(|error| error.to_string())?;
    let turn = timeline
        .capture_turn(character, None, timeline.anchor())
        .map_err(|error| error.to_string())?;
    if !knowledge.needs_authoritative_refresh(character, turn) {
        return Ok(());
    }
    let (_selection, anchor, world) = timeline
        .selected_world_snapshot()
        .map_err(|error| error.to_string())?;
    if anchor.through != turn.context_head {
        return Err("Timeline moved while reconciling knowledge".into());
    }
    let memories = crate::character::projection::character_memories_at(&world, character)
        .map_err(|error| error.to_string())?
        .all();
    let after = timeline
        .capture_turn(character, None, timeline.anchor())
        .map_err(|error| error.to_string())?;
    if turn != after {
        return Err("Timeline advanced during knowledge reconciliation".into());
    }
    knowledge
        .refresh_authoritative(character, turn, &memories)
        .map_err(|error| error.to_string())
}

async fn run_semantic_knowledge_reconciler(
    timeline: Arc<Mutex<SessionTimeline>>,
    knowledge: Arc<KnowledgeRetrieval>,
    character: CharacterId,
    mut memories: tokio::sync::watch::Receiver<u64>,
    mut context: tokio::sync::watch::Receiver<u64>,
    stop: CancellationToken,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            changed = memories.changed() => if changed.is_err() { break; },
            changed = context.changed() => if changed.is_err() { break; },
            _ = interval.tick() => {},
        }
        if let Err(error) = reconcile_semantic_knowledge(&timeline, &knowledge, character) {
            tracing::warn!(%character, %error, "authoritative semantic cache refresh deferred");
        }
    }
}

struct TimelineRequestIngress {
    timeline: Arc<Mutex<SessionTimeline>>,
}

impl ProviderDispatchFence for TimelineRequestIngress {
    fn validate_knowledge(
        &self,
        turn: crate::timeline::selection::TimelineTurn,
        latest: bool,
    ) -> Result<(), String> {
        self.timeline
            .lock()
            .map_err(|_| String::from("timeline mutex poisoned"))?
            .assert_knowledge_turn(turn, latest)
            .map_err(|error| error.to_string())
    }

    fn before_provider(
        &self,
        request: RequestId,
        admission: TimelineAdmission,
        protocol: Option<RegenerateProtocol>,
    ) -> Result<(), String> {
        if protocol.is_none() {
            return Ok(());
        }
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| "timeline mutex poisoned".to_owned())?;
        let child = admission.selection.branch_id;
        if protocol == Some(RegenerateProtocol::SameBranch) {
            return timeline
                .permit_same_branch_regenerate_dispatch(child, request, admission.selection)
                .map(|_| ())
                .map_err(|error| error.to_string());
        }
        let Some(receipt) = timeline
            .regenerate_admission_receipt(child)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        if receipt.request != request {
            return Ok(());
        }
        timeline
            .permit_regenerate_dispatch(child, request, admission.selection)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

impl RegenerateCancellation for TimelineRequestIngress {
    fn abandon_regenerate(
        &self,
        request: RequestId,
        admission: TimelineAdmission,
    ) -> Result<(), crate::timeline::domain::TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| {
                crate::timeline::domain::TimelineError::Storage(
                    "timeline mutex poisoned".to_owned(),
                )
            })?
            .abandon_regenerate_request(admission.selection.branch_id, request, admission.selection)
            .map(|_| ())
    }
    fn inspect_regenerate(
        &self,
        child: BranchId,
        request: RequestId,
    ) -> Result<crate::timeline::RegenerateRecoverySnapshot, crate::timeline::domain::TimelineError>
    {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline mutex poisoned".to_owned()))?
            .reconcile_regenerate(child, request)
    }

    fn abandon_same_branch_regenerate(
        &self,
        request: RequestId,
        admission: TimelineAdmission,
    ) -> Result<(), TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline mutex poisoned".to_owned()))?
            .abandon_same_branch_regenerate_request(admission.selection.branch_id, request)
            .map(|_| ())
    }

    fn inspect_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, TimelineError> {
        self.timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline mutex poisoned".to_owned()))?
            .reconcile_same_branch_regenerate(branch, request)
            .map(|snapshot| snapshot.outcome)
    }

    fn abandon_orphaned_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, TimelineError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline mutex poisoned".to_owned()))?;
        let current = timeline
            .reconcile_same_branch_regenerate(branch, request)?
            .outcome;
        if matches!(
            current,
            RegenerateRecoveryOutcome::AcceptedReply(_) | RegenerateRecoveryOutcome::Abandoned(_)
        ) {
            return Ok(current);
        }
        match timeline.abandon_same_branch_regenerate_request(branch, request) {
            Ok(event) => Ok(RegenerateRecoveryOutcome::Abandoned(event)),
            Err(error) => {
                if matches!(
                    error,
                    TimelineError::Conflict | TimelineError::ConcurrentWrite
                ) && let Ok(snapshot) =
                    timeline.reconcile_same_branch_regenerate(branch, request)
                    && matches!(
                        snapshot.outcome,
                        RegenerateRecoveryOutcome::AcceptedReply(_)
                            | RegenerateRecoveryOutcome::Abandoned(_)
                    )
                {
                    return Ok(snapshot.outcome);
                }
                Err(error)
            }
        }
    }
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
        fence: Option<crate::request::TimelineFence>,
    ) -> Result<TimelineAdmission, SubmitError> {
        let timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        let selection = timeline.selection();
        timeline
            .assert_selection(selection)
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        if fence.is_some_and(|expected| {
            expected.selection != selection || expected.at != timeline.anchor()
        }) {
            return Err(SubmitError::StaleTimeline);
        }
        if let TurnIntent::Reply(_) | TurnIntent::React(_) = intent {
            timeline
                .reaction_cause_at(character, intent, timeline.anchor())
                .map_err(|_| SubmitError::InvalidTargetEvent)?;
        }
        timeline
            .admission(character, selection)
            .map_err(|_| SubmitError::TimelineUnavailable)
    }

    fn admit_regenerate(
        &self,
        source: RegenerateSource,
        fence: crate::request::TimelineFence,
        child: BranchId,
        receipt: &crate::timeline::RegenerateAdmissionReceipt,
    ) -> Result<TimelineAdmission, SubmitError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        if fence.at.branch_id != fence.selection.branch_id.0 {
            return Err(SubmitError::StaleTimeline);
        }
        timeline
            .fork_and_admit_regenerate_source(
                source,
                child,
                fence.selection,
                fence.at.through,
                receipt,
            )
            .map_err(|error| match error {
                TimelineError::ConcurrentWrite => SubmitError::StaleTimeline,
                TimelineError::Invalid(_) | TimelineError::NotFound | TimelineError::Conflict => {
                    SubmitError::InvalidTargetEvent
                }
                _ => SubmitError::TimelineUnavailable,
            })
    }

    fn admit_same_branch_regenerate(
        &self,
        source: RegenerateSource,
        fence: crate::request::TimelineFence,
        request: RequestId,
        configuration: RegenerateConfigurationReference,
    ) -> Result<TimelineAdmission, SubmitError> {
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| SubmitError::TimelineUnavailable)?;
        if fence.selection.branch_id.0 != fence.at.branch_id {
            return Err(SubmitError::StaleTimeline);
        }
        let plan = timeline
            .plan_knowledge_regenerate(source, fence.selection, fence.at.through)
            .map_err(|error| match error {
                TimelineError::ConcurrentWrite => SubmitError::StaleTimeline,
                TimelineError::Invalid(_) | TimelineError::NotFound | TimelineError::Conflict => {
                    SubmitError::InvalidTargetEvent
                }
                _ => SubmitError::TimelineUnavailable,
            })?;
        let receipt = SameBranchRegenerateReceipt {
            request,
            accepted_request: source.accepted_request,
            accepted_message: source.accepted_message,
            original_input: source.original_input,
            character: source.character,
            branch: fence.selection.branch_id,
            source_head: plan.head,
            selection_generation: fence.selection.generation,
            hide_operation: EventId::new(),
            forget_operation: (!plan.forget_events.is_empty()).then(EventId::new),
            hidden_messages: plan.hide_messages.clone(),
            forgotten_events: plan.forget_events.clone(),
            configuration,
        };
        timeline
            .admit_same_branch_regenerate(&plan, &receipt, EventId::new())
            .map_err(|error| match error {
                TimelineError::ConcurrentWrite => SubmitError::StaleTimeline,
                TimelineError::Invalid(_) | TimelineError::NotFound | TimelineError::Conflict => {
                    SubmitError::InvalidTargetEvent
                }
                _ => SubmitError::TimelineUnavailable,
            })
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
    game_link_task: Option<JoinHandle<std::io::Result<()>>>,
    lease: RuntimeLease,
    _settings_instance: Option<Arc<SettingsInstanceLease>>,
    ai_runtime: Option<RunningAiApplication>,
    prepared_claims: Vec<WorkerClaim>,
    ai_hub_install: Option<Arc<AiHubInstallCoordinator>>,
    ai_hub_native: Option<Arc<AiHubNativeService>>,
    live_speech: Option<Arc<LiveSpeechRuntime>>,
    speech_watch_task: Option<JoinHandle<()>>,
}

impl ApplicationLifecycle {
    pub async fn start(config: ApplicationConfig) -> Result<Self, ApplicationError> {
        Box::pin(Self::start_inner(config)).await
    }

    async fn start_inner(config: ApplicationConfig) -> Result<Self, ApplicationError> {
        let lease = RuntimeLease::reserve().ok_or(ApplicationError::Busy)?;
        let id = lease.id();
        let ApplicationConfig {
            releases: release_feed,
            migrations,
            presets,
            prompts,
            generator,
            knowledge,
            graph_extractor,
            limits,
            voice,
            ai_runtime,
            ai_hub_install_root,
            ai_speech_inventory,
            ai_installed_speech,
            ai_hardware,
            ai_knowledge,
            mut settings,
            settings_instance,
            timeline,
            timeline_assets,
            server,
            game_link,
            game_beats,
            finetune,
            games,
            reminders: reminders_config,
        } = config;
        let settings_instance = match (settings_instance, settings.repository.lifecycle_root()) {
            (Some(instance), Some(root))
                if instance
                    .matches_root(root)
                    .map_err(|error| SettingsError::Storage(error.to_string()))? =>
            {
                Some(instance)
            }
            (Some(_), _) => {
                return Err(SettingsError::Storage(
                    "persistent settings root changed after application configuration".into(),
                )
                .into());
            }
            (None, Some(root)) => Some(Arc::new(
                SettingsInstanceLease::acquire(root)
                    .map_err(|error| SettingsError::Storage(error.to_string()))?,
            )),
            (None, None) => None,
        };
        if let Some(instance) = &settings_instance
            && let Some(path) = settings.repository.json_path_for_owned_lifecycle()
        {
            settings.repository = Arc::new(LeasedJsonSettingsRepository::new(
                path.to_path_buf(),
                Arc::clone(instance),
            ));
        }
        if let Some(root) = &ai_hub_install_root {
            if !root.is_absolute() {
                return Err(SettingsError::Storage(
                    "AI Hub must use an absolute owned data root".into(),
                )
                .into());
            }
            if let Some(instance) = &settings_instance
                && !instance
                    .matches_root(root)
                    .map_err(|error| SettingsError::Storage(error.to_string()))?
            {
                return Err(SettingsError::Storage(
                    "AI Hub must share the exclusive Core-owned profile root".into(),
                )
                .into());
            }
        }
        if let (Some(instance), Some(environment)) = (&settings_instance, &ai_installed_speech)
            && !instance
                .matches_root(&environment.data_directory)
                .map_err(|error| SettingsError::Storage(error.to_string()))?
        {
            return Err(SettingsError::Storage(
                "installed speech and persisted settings must share the same owned data root"
                    .into(),
            )
            .into());
        }
        let service_workers = ServiceWorkerRegistry::new();
        let ai_hub_install = ai_hub_install_root
            .as_ref()
            .map(|root| AiHubInstallCoordinator::new(root.clone()));
        let ai_hub_native = ai_hub_install_root
            .map(|root| AiHubNativeService::with_registry(root, Arc::clone(&service_workers)));
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
        let mut speech_startup_warning = None;
        let live_environment = ai_installed_speech.as_ref().cloned();
        let auto_speech = live_environment.is_some();
        let ai_runtime = match (ai_runtime, ai_speech_inventory, ai_installed_speech) {
            (Some(plan), Some(inventory), None) => {
                let saved = settings_owner.snapshot_all()?.values.speech.clone();
                Some(plan.with_speech_settings(&saved, &inventory).await?)
            }
            (Some(plan), None, None) => Some(plan),
            (None, None, Some(environment)) => {
                let mut saved = settings_owner.snapshot_all()?.values.speech.clone();
                if !saved.tts.local_auto_load {
                    saved.tts.enabled = false;
                }
                match environment.prepare_from_saved(&saved).await {
                    Ok(plan) => plan,
                    Err(error) => {
                        speech_startup_warning = Some(error.to_string());
                        None
                    }
                }
            }
            (None, None, None) => None,
            _ => return Err(AiApplicationError::Invalid(
                "speech runtime must use either an explicit prepared plan or installed discovery"
                    .into(),
            )
            .into()),
        };
        if ai_runtime
            .as_ref()
            .is_some_and(|plan| plan.topology != crate::ai_runtime::ProcessTopology::Split)
        {
            return Err(AiApplicationError::Invalid(
                "Core requires one independent worker process per AI service".into(),
            )
            .into());
        }
        let (server_lifecycle, server) = ServerLifecycle::bind(server).await?;
        let game_link = match game_link {
            Some(config) => Some(GameLinkLifecycle::bind_with_beats(config, game_beats).await?),
            None => None,
        };
        let timeline = Arc::new(Mutex::new(SessionTimeline::from_config(timeline)?));
        let mut prepared_claims = Vec::new();
        if !(auto_speech && ai_hardware.is_some())
            && let Some(configuration) = ai_runtime.as_ref()
        {
            for worker in &configuration.workers {
                for selected in &worker.models {
                    if matches!(
                        selected.service,
                        AiService::Embedding | AiService::Reranker | AiService::Beats
                    ) {
                        prepared_claims.push(
                            service_workers
                                .claim(ServiceModel {
                                    service: selected.service,
                                    model: AiModelIdentity {
                                        model_id: selected.plugin_id.clone(),
                                        implementation: WorkerImplementation::Python,
                                        revision: selected.asset_revision.clone(),
                                    },
                                })
                                .map_err(|error| AiApplicationError::Invalid(error.to_string()))?,
                        );
                    }
                }
            }
        }
        let mut ai_runtime = match (ai_runtime, ai_hardware) {
            (Some(configuration), Some(path)) if auto_speech => {
                speech_startup_warning = Some(format!(
                    "automatic speech activation cannot combine with hardware selection at {}",
                    path.display()
                ));
                let _ = configuration;
                None
            }
            (Some(configuration), Some(path)) => {
                let store = crate::ai_runtime::hardware::HardwareConfigStore::new(path);
                let selected = store.load().map_err(|error| {
                    crate::ai_runtime::application::AiApplicationError::Invalid(error.to_string())
                })?;
                Some(Box::pin(configuration.activate_with_hardware(&selected, &store)).await?)
            }
            (Some(configuration), None) if auto_speech => {
                match Box::pin(configuration.activate()).await {
                    Ok(running) => Some(running),
                    Err(error) => {
                        speech_startup_warning = Some(error.to_string());
                        None
                    }
                }
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
        if let Some(runtime) = ai_runtime.as_ref() {
            for service in [AiService::Embedding, AiService::Reranker, AiService::Beats] {
                if runtime.has_service(service) {
                    let loaded = runtime
                        .status_snapshot()
                        .service(service)
                        .active_model
                        .ok_or_else(|| {
                            AiApplicationError::Invalid(format!(
                                "running {service:?} worker has no active model"
                            ))
                        })?;
                    let actual = ServiceModel {
                        service,
                        model: AiModelIdentity {
                            model_id: loaded.plugin_id,
                            implementation: WorkerImplementation::Python,
                            revision: loaded.asset_revision,
                        },
                    };
                    if service_workers
                        .active(service)
                        .map_err(|error| AiApplicationError::Invalid(error.to_string()))?
                        != Some(actual)
                    {
                        return Err(AiApplicationError::Invalid(format!(
                            "{service:?} activation did not match its pre-spawn worker claim"
                        ))
                        .into());
                    }
                }
            }
        } else {
            prepared_claims.clear();
        }
        let knowledge = match ai_knowledge {
            None => knowledge,
            Some(selection) => {
                let runtime = ai_runtime
                    .as_ref()
                    .ok_or(ApplicationError::MissingEmbeddingWorker)?;
                let adapter = runtime
                    .embedding_provider(selection.space)
                    .ok_or(ApplicationError::MissingEmbeddingWorker)?;
                let provider: Arc<dyn EmbeddingProvider> = Arc::new(ServiceEmbeddingProvider::new(
                    Arc::clone(&service_workers),
                    service_workers
                        .active(AiService::Embedding)
                        .map_err(|error| EmbeddingError::Transport(error.to_string()))?
                        .ok_or(ApplicationError::MissingEmbeddingWorker)?,
                    Arc::new(adapter),
                )?);
                let mut retriever =
                    SemanticMemoryRetriever::new(provider, selection.cache_entries)?;
                if let Some(reranker) = selection.reranker {
                    let adapter = runtime
                        .reranker(reranker.model_id, reranker.protocol)
                        .ok_or(ApplicationError::MissingRerankerWorker)?
                        .map_err(|error| EmbeddingError::Configuration(error.to_string()))?;
                    let scorer: Arc<dyn RerankProvider> = Arc::new(ServiceRerankProvider::new(
                        Arc::clone(&service_workers),
                        service_workers
                            .active(AiService::Reranker)
                            .map_err(|error| EmbeddingError::Transport(error.to_string()))?
                            .ok_or(ApplicationError::MissingRerankerWorker)?,
                        Arc::new(adapter),
                    )?);
                    retriever = retriever.with_reranker(scorer, reranker.policy)?;
                }
                KnowledgeRetrieval::semantic_fusion(Arc::new(retriever), selection.fusion)?
            }
        };
        knowledge.bind_dispatch_authority({
            let timeline = Arc::clone(&timeline);
            #[cfg(feature = "sqlite-system")]
            let repository = timeline
                .lock()
                .map_err(|_| EmbeddingError::StaleKnowledge)?
                .dispatch_repository();
            Arc::new(move |turn, sources| {
                let timeline = Arc::clone(&timeline);
                #[cfg(feature = "sqlite-system")]
                let repository = repository.clone();
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || {
                        #[cfg(feature = "sqlite-system")]
                        if let Some(repository) = repository {
                            return repository
                                .admit_knowledge_dispatch(turn, &sources)
                                .map(crate::timeline::KnowledgeDispatchLease::from_sqlite)
                                .map_err(|_| EmbeddingError::StaleKnowledge);
                        }
                        timeline
                            .lock()
                            .map_err(|_| EmbeddingError::StaleKnowledge)?
                            .admit_knowledge_dispatch(turn, &sources)
                            .map_err(|_| EmbeddingError::StaleKnowledge)
                    })
                    .await
                    .map_err(|error| EmbeddingError::Transport(error.to_string()))?
                })
            })
        })?;
        // A stable speech boundary is needed even if no worker was installed at
        // startup. The worker may appear or change after settings are edited.
        let initial_speech = settings_owner.snapshot_all()?.values.speech.clone();
        let live_speech = live_environment.map(|environment| {
            Arc::new(
                LiveSpeechRuntime::new_with_registry(
                    environment,
                    ai_runtime
                        .as_mut()
                        .map(|runtime| runtime.take_services(&[AiService::Tts, AiService::Asr])),
                    &initial_speech,
                    speech_startup_warning.as_deref(),
                    Arc::clone(&service_workers),
                )
                .expect("no native worker can be active before application startup finishes")
                .with_native(ai_hub_native.clone()),
            )
        });
        if live_speech.is_none()
            && let Some(runtime) = ai_runtime.as_ref()
        {
            for service in [AiService::Tts, AiService::Asr] {
                if runtime.has_service(service) {
                    let model = runtime
                        .status_snapshot()
                        .service(service)
                        .active_model
                        .map(|loaded| {
                            crate::settings::SpeechModelReference::python(loaded.plugin_id)
                        })
                        .ok_or_else(|| {
                            AiApplicationError::Invalid(
                                "running Python worker has no active model".into(),
                            )
                        })?;
                    prepared_claims.push(
                        service_workers
                            .claim(ServiceModel {
                                service,
                                model: model.into(),
                            })
                            .map_err(|error| AiApplicationError::Invalid(error.to_string()))?,
                    );
                }
            }
        }
        let ai_recovery = ai_runtime
            .as_ref()
            .map(RunningAiApplication::recovery_handle);
        let speech: Option<Arc<dyn crate::audio::session::SpeechTranscriber>> =
            if let Some(live) = &live_speech {
                Some(Arc::new(LiveSpeechTranscriber(Arc::clone(live))))
            } else {
                ai_runtime
                    .as_ref()
                    .and_then(|running| running.speech_recognizer())
                    .map(|recognizer| {
                        Arc::new(recognizer) as Arc<dyn crate::audio::session::SpeechTranscriber>
                    })
            };
        let voice: Arc<dyn VoiceProvider> = if let Some(live) = &live_speech {
            Arc::new(LiveSpeechVoice(Arc::clone(live)))
        } else {
            match &ai_runtime {
                Some(running) if running.has_service(AiService::Tts) => running.voice_provider(),
                _ => voice,
            }
        };
        let speech: Option<Arc<dyn crate::audio::session::SpeechTranscriber>> =
            if speech.is_some() || ai_hub_native.is_some() {
                Some(Arc::new(RoutedSpeechTranscriber::new(
                    Arc::clone(&settings_owner),
                    speech,
                    ai_hub_native.clone(),
                    Arc::clone(&service_workers),
                )))
            } else {
                None
            };
        let voice: Arc<dyn VoiceProvider> = Arc::new(RoutedSpeechVoice::new(
            Arc::clone(&settings_owner),
            voice,
            ai_hub_native.clone(),
            Arc::clone(&service_workers),
        ));
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
        let (characters, mut owners) = CharacterRegistry::spawn_with_reminders(
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
        for id in CharacterId::ALL {
            let (memory_updates, context_updates) = {
                let timeline = timeline.lock().expect("timeline lock poisoned");
                (
                    timeline.observe_memory_changes(id),
                    timeline.observe_context_epoch(id),
                )
            };
            owners.push(tokio::spawn(run_semantic_knowledge_reconciler(
                Arc::clone(&timeline),
                Arc::clone(&knowledge),
                id,
                memory_updates,
                context_updates,
                owners_stop.clone(),
            )));
        }
        let games = Arc::new(games);
        for id in CharacterId::ALL {
            characters
                .get(id)
                .expect("hardcoded character exists")
                .configure_game_policy(*games)
                .await
                .map_err(|error| ApplicationError::Task(error.to_string()))?;
        }
        let shared_ingress = Arc::new(TimelineRequestIngress {
            timeline: Arc::clone(&timeline),
        });
        let dispatch_fence: Arc<dyn ProviderDispatchFence> = shared_ingress.clone();
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
                prompt_preparation: crate::request::prompt_preparation::PromptPreparation::new(),
                finetune: finetune.clone(),
                selection_updates: selection_updates.clone(),
                context_updates,
                games: Arc::clone(&games),
                assets: timeline_assets.clone(),
                dispatch_fence: Arc::clone(&dispatch_fence),
            };
            workers.push(tokio::spawn(
                worker.run_queue(receive, workers_stop.clone()),
            ));
        }
        let ingress: Arc<dyn RequestIngress> = shared_ingress;
        let api = Arc::new(ChatApiState::new(
            registry,
            handlers,
            ingress,
            Arc::clone(&settings_owner),
            Arc::clone(&presets),
            prompts.clone(),
        ));
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
            timeline: Arc::clone(&timeline),
            characters,
            knowledge: Arc::clone(&knowledge),
            graph_extractor,
            settings: settings_owner,
            presets: Arc::clone(&presets),
            games: Arc::clone(&games),
            reminders,
        });
        let mut ai_runtime_status = if let Some(live) = &live_speech {
            let mut speech = live.observe();
            let updates = if let Some(runtime) = &ai_runtime {
                let mut other = runtime.observe_status();
                let merge = |speech: &crate::ai_runtime::runtime_status::RuntimeSnapshot, other: &crate::ai_runtime::runtime_status::RuntimeSnapshot| {
                    let mut combined = other.clone();
                    combined.services.retain(|service, _| !matches!(service, AiService::Tts | AiService::Asr));
                    combined.services.extend(speech.services.clone());
                    combined
                };
                let (send, receive) =
                    tokio::sync::watch::channel(merge(&speech.borrow(), &other.borrow()));
                let stop = owners_stop.clone();
                owners.push(tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = stop.cancelled() => break,
                            changed = speech.changed() => if changed.is_err() { break; },
                            changed = other.changed() => if changed.is_err() { break; },
                        }
                        let combined =
                            merge(&speech.borrow_and_update(), &other.borrow_and_update());
                        send.send_if_modified(|current| {
                            if *current == combined {
                                false
                            } else {
                                *current = combined;
                                true
                            }
                        });
                    }
                }));
                receive
            } else {
                speech
            };
            crate::presentation::RuntimeStatePresenter::new(Some(updates))
                .with_live_speech(Arc::downgrade(live))
        } else {
            crate::presentation::RuntimeStatePresenter::new(
                ai_runtime.as_ref().map(|runtime| runtime.observe_status()),
            )
        };
        if let Some(runtime) = &ai_runtime {
            ai_runtime_status = ai_runtime_status.with_supervisor(runtime.supervisor_weak());
        } else if live_speech.is_none()
            && let Some(reason) = &speech_startup_warning
        {
            let speech = bindings.settings.snapshot_all()?.values.speech.clone();
            let mut services = Vec::new();
            if speech.tts.enabled
                && speech.tts.local_auto_load
                && speech.tts.backend == crate::settings::TtsBackend::Local
            {
                services.push(crate::ai_runtime::AiService::Tts);
            }
            if !speech.asr.recognizer_id.is_empty() {
                services.push(crate::ai_runtime::AiService::Asr);
            }
            ai_runtime_status = ai_runtime_status.with_startup_failure(&services, reason);
        }
        let speech_controller = Arc::new(tokio::sync::Mutex::new(
            crate::audio::controller::SpeechSessionController::default(),
        ));
        let speech_view = crate::presentation::SpeechSessionPresenter::new(
            speech_controller.lock().await.observe(),
        );
        let audio_input = Arc::new(crate::audio::input_owner::AudioInputOwner::new(Arc::clone(
            &speech_controller,
        )));
        let speech_watch_task = live_speech.as_ref().map(|live| {
            tokio::spawn(watch_speech_settings(
                Arc::clone(live),
                Arc::clone(&bindings.settings),
                Arc::clone(&speech_controller),
                bindings.settings.subscribe(),
                initial_speech.clone(),
                owners_stop.child_token(),
            ))
        });
        let game_link_handle = game_link.as_ref().map(|(_, handle)| handle.clone());
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
            game_link_task: None,
            lease,
            _settings_instance: settings_instance,
            ai_runtime,
            prepared_claims,
            ai_hub_install: ai_hub_install.clone(),
            ai_hub_native: ai_hub_native.clone(),
            live_speech,
            speech_watch_task,
        };
        owner.lease.publish(Arc::clone(&bindings));
        if let Some((lifecycle, _handle)) = game_link {
            owner.game_link_task = Some(tokio::spawn(
                lifecycle.run_with_api(owner.server_stop.clone(), Arc::clone(&bindings)),
            ));
        }
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
                audio_input: crate::presentation::AudioInputPresenter::new(Arc::clone(
                    &audio_input,
                )),
                prompts: prompts.clone(),
                speech: speech_view,
                ai_runtime: ai_runtime_status,
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
            game_link: game_link_handle,
            finetune,
            speech,
            audio_input,
            speech_controller,
            speech_startup_warning,
            ai_recovery,
            recovery_workers: Arc::clone(&service_workers),
            ai_hub_install,
            ai_hub_native,
            id,
            stop,
            completion: Some(completion),
        })
    }

    /// Explicitly restore a failed prepared Python worker, checked against its
    /// original generation and selected model plan. Does not auto-retry requests.
    pub async fn restart_failed_ai_worker(
        &self,
        service: AiService,
        expected_generation: Uuid,
    ) -> Result<Uuid, AiApplicationError> {
        if self.stop.is_cancelled() {
            return Err(AiApplicationError::Invalid(
                "application is stopping".into(),
            ));
        }
        let recovery = self.ai_recovery.as_ref().ok_or_else(|| {
            AiApplicationError::Invalid("no explicit AI worker recovery is available".into())
        })?;
        let _transition = self.recovery_workers.transition_gate(service).await;
        recovery
            .restart_failed_worker(service, expected_generation)
            .await
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
            Arc::clone(recognizer),
            Arc::new(crate::audio::session::ChatReplyTarget::bound(
                Arc::clone(&self.bindings.chat),
                character,
                source,
            )),
            playback,
        );
        Some((handler, self.stop.child_token()))
    }

    /// Starts a microphone session with a caller-owned transcript destination.
    /// Desktop routing uses this rather than implicitly submitting every transcript.
    pub async fn start_speech_capture_routed(
        &self,
        config: crate::audio::controller::SpeechCaptureConfig,
        conversation: Arc<dyn crate::audio::session::SpeechConversation>,
    ) -> Result<(), crate::audio::controller::SpeechCaptureError> {
        let runtime = self.presenter.ai_runtime.refresh_status().await;
        if runtime.service(crate::ai_runtime::AiService::Asr).phase
            != crate::ai_runtime::runtime_status::RuntimePhase::ModelReady
        {
            return Err(crate::audio::controller::SpeechCaptureError::Invalid(
                "ASR model must be initialized before capturing audio".into(),
            ));
        }
        let recognizer = self.speech.as_ref().ok_or_else(|| {
            crate::audio::controller::SpeechCaptureError::Invalid(
                "ASR worker has not been activated".into(),
            )
        })?;
        let handler = crate::audio::session::SpeechInputHandler::new(
            Arc::clone(recognizer),
            conversation,
            crate::audio::controller::playback(&config),
        );
        self.audio_input
            .start_capture(handler, self.stop.child_token(), config)
            .await
    }

    /// Starts capture only if an ASR worker was explicitly provided to this application.
    pub async fn start_speech_capture(
        &self,
        config: crate::audio::controller::SpeechCaptureConfig,
        character: CharacterId,
    ) -> Result<(), crate::audio::controller::SpeechCaptureError> {
        let runtime = self.presenter.ai_runtime.refresh_status().await;
        if runtime.service(crate::ai_runtime::AiService::Asr).phase
            != crate::ai_runtime::runtime_status::RuntimePhase::ModelReady
        {
            return Err(crate::audio::controller::SpeechCaptureError::Invalid(
                "ASR model must be initialized before capturing audio".into(),
            ));
        }
        let playback = crate::audio::controller::playback(&config);
        let (handler, stop) = self
            .speech_session(
                character,
                crate::request::RequestSource::Presenter,
                playback,
            )
            .ok_or_else(|| {
                crate::audio::controller::SpeechCaptureError::Invalid(
                    "ASR worker has not been activated".into(),
                )
            })?;
        self.audio_input.start_capture(handler, stop, config).await
    }

    pub async fn stop_speech_capture(&self) {
        self.audio_input.stop_capture().await;
    }

    pub async fn press_speech_button(
        &self,
        pressed: bool,
    ) -> Result<(), crate::audio::controller::SpeechCaptureError> {
        self.speech_controller.lock().await.press(pressed).await
    }

    pub fn speech_startup_warning(&self) -> Option<&str> {
        self.speech_startup_warning.as_deref()
    }

    pub fn ai_hub_native(&self) -> Option<&Arc<AiHubNativeService>> {
        self.ai_hub_native.as_ref()
    }

    pub fn ai_hub_install(&self) -> Option<&Arc<AiHubInstallCoordinator>> {
        self.ai_hub_install.as_ref()
    }

    pub fn presenter(&self) -> &GuiPresenter {
        &self.presenter
    }
    pub fn game_link(&self) -> Option<&GameLinkHandle> {
        self.game_link.as_ref()
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

    /// Publish an attachment-bearing event only after verifying all binary
    /// references, under the same Timeline selection lock as the canonical write.
    /// An unsuccessful append may leave an orphan object; it never records a
    /// receipt for an unavailable reference. The asset root is injected.
    pub fn publish_attached_conversation_message(
        &self,
        expected: TimelineSelection,
        message: ConversationMessage,
        assets: &crate::timeline::storage::assets::TimelineAssetStore,
    ) -> Result<TimelineFact, crate::timeline::storage::assets::TimelineAttachmentCommitError> {
        use crate::timeline::{
            domain::parse_message_content,
            storage::assets::{TimelineAssetError, TimelineAttachmentCommitError},
        };
        let mut timeline = self
            .timeline
            .lock()
            .map_err(|_| TimelineError::Storage("timeline lock poisoned".into()))?;
        timeline.assert_selection(expected)?;
        let parts = parse_message_content(&message.content)
            .map_err(|_| TimelineAssetError::InvalidReference)?;
        if !parts.iter().any(|part| part.asset().is_some()) {
            return Err(TimelineAttachmentCommitError::Asset(
                TimelineAssetError::InvalidReference,
            ));
        }
        for part in parts {
            if let Some(reference) = part.asset() {
                assets.verify(reference)?;
            }
        }
        Ok(timeline.publish_conversation_message(expected, message)?)
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
        self.stop_speech_capture().await;
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
        if let Some(task) = self.game_link_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failure = Some(ApplicationError::Server(error)),
                Err(error) => failure = Some(ApplicationError::Task(error.to_string())),
            }
        }
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
        if let Some(task) = self.speech_watch_task.take()
            && let Err(error) = task.await
        {
            failure = Some(ApplicationError::Task(error.to_string()));
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
        if let Some(native) = self.ai_hub_native.take() {
            native.shutdown().await;
        }
        if let Some(hub) = self.ai_hub_install.take() {
            hub.shutdown().await;
        }
        if let Some(runtime) = self.ai_runtime.take()
            && let Err(error) = runtime.shutdown().await
        {
            failure = Some(ApplicationError::Ai(error));
        }
        if let Some(live) = self.live_speech.take()
            && let Err(error) = live.shutdown().await
        {
            failure = Some(ApplicationError::Ai(error));
        }
        self.prepared_claims.clear();
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
        if let Some(task) = &self.game_link_task {
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

#[cfg(test)]
#[path = "app/regenerate_admission_tests.rs"]
mod regenerate_admission_tests;

#[cfg(test)]
#[path = "app/regenerate_b86_tests.rs"]
mod regenerate_b86_tests;

#[cfg(test)]
mod settings_instance_tests {
    use super::*;
    use crate::generation::GenerationProvider;
    use crate::settings::JsonSettingsRepository;

    fn config(root: &Path) -> Result<ApplicationConfig, ApplicationError> {
        ApplicationConfig::persistent_unconfigured(
            ResponseGenerator::new(Vec::<Arc<dyn GenerationProvider>>::new()).unwrap(),
            RuntimeLimits {
                request_queue: NonZeroUsize::new(2).unwrap(),
                discovery_buffer: NonZeroUsize::new(2).unwrap(),
                character_commands: NonZeroUsize::new(2).unwrap(),
                presenter_updates: NonZeroUsize::new(2).unwrap(),
                retained_finished: NonZeroUsize::new(2).unwrap(),
            },
            "127.0.0.1:0".parse().unwrap(),
            root,
        )
    }

    #[test]
    fn portable_application_configs_cannot_own_the_same_root_concurrently() {
        let root = std::env::temp_dir().join(format!("nm-app-owner-{}", Uuid::new_v4()));
        let first = config(&root).unwrap();
        assert!(matches!(
            config(&root),
            Err(ApplicationError::Settings(SettingsError::Storage(message)))
                if message.contains("lock") || message.contains("used")
        ));
        drop(first);
        assert!(config(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subprocess_persistent_config_fixture() {
        let Ok(path) = std::env::var("NM_APP_INSTANCE_CHILD_ROOT") else {
            return;
        };
        let ready = std::env::var("NM_APP_INSTANCE_CHILD_READY").unwrap();
        let _owner = config(Path::new(&path)).unwrap();
        std::fs::write(ready, "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    fn second_os_process_cannot_prepare_the_same_persisted_app_and_crash_reopens_it() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let root = std::env::temp_dir().join(format!("nm-app-process-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let ready = root.join("ready");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::settings_instance_tests::subprocess_persistent_config_fixture",
            ])
            .env("NM_APP_INSTANCE_CHILD_ROOT", &root)
            .env("NM_APP_INSTANCE_CHILD_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "other application did not prepare"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "other application exited"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(
            config(&root),
            Err(ApplicationError::Settings(SettingsError::Storage(_)))
        ));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(config(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn startup_refuses_retargeted_settings_root_without_releasing_original_owner() {
        let root = std::env::temp_dir().join(format!("nm-app-retarget-{}", Uuid::new_v4()));
        let foreign = std::env::temp_dir().join(format!("nm-app-foreign-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&foreign).unwrap();
        let mut configured = config(&root).unwrap();
        configured.settings.repository =
            Arc::new(JsonSettingsRepository::new(foreign.join("settings.json")));
        assert!(matches!(
            ApplicationLifecycle::start(configured).await,
            Err(ApplicationError::Settings(SettingsError::Storage(_)))
        ));
        assert!(config(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(foreign).unwrap();
    }
}
