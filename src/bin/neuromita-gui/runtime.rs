use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, thread, time::Duration};

use neuromita::{
    ai_runtime::{
        install_admission::{InstallAction, InstallActionReview},
        install_coordinator::InstallEvent,
        install_review::{InstallPlanReview, InstallPreviewError},
        native_download_operation::{NativeDownloadEvent, NativeDownloadSelection},
        native_https::NativeTransferPhase,
        native_model::NativeBackend,
        native_revisions::NativeRevisionOverview,
        native_service::AiHubNativeService,
        native_trust::NativeAuthorizedRelease,
        native_workflow::NativeWorkflowSnapshot,
        runtime_status::{RuntimePhase, RuntimeSnapshot},
        speech::Transcript,
        speech_catalog::{SpeechInventory, SpeechModelCatalog},
    },
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    audio::{
        controller::{SpeechCaptureConfig, SpeechSessionSnapshot},
        input_catalog::AudioInputDevice,
        monitor::MonitorSnapshot,
        session::{SpeechConversation, SpeechFuture, SpeechSessionError},
    },
    character::CharacterId,
    generation::PresetRecord,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    presentation::{
        ConversationChange, ConversationChangePreview, ConversationSnapshot, GuiPresenter,
        PresetSlot, native_download::NativeDownloadPresenter,
    },
    request::{CancelResult, RequestId},
    settings::{
        AsrPreferences, GuideLevel, InterfaceSettings, OnboardingSettings, PresetId,
        PresetSelection, SettingsApi, SettingsError, TtsPreferences, Volume,
    },
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOption {
    Enabled,
    Calculator,
    MemorySearch,
    WebSearch,
    WebReader,
}

#[path = "chat_fence.rs"]
pub mod chat_fence;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideChange {
    Page(u8),
    Level(GuideLevel),
    Language(neuromita::settings::Language),
    Complete,
}

impl GuideChange {
    fn edit_onboarding(self, state: &mut OnboardingSettings) {
        match self {
            Self::Page(page) => state.guide_page = page,
            Self::Level(level) => {
                state.guide_level = level;
                state.guide_page = 0;
            }
            Self::Complete => state.guide_completed = true,
            Self::Language(_) => {}
        }
    }

    fn apply(self) -> Result<(), SettingsError> {
        match self {
            Self::Page(page) => {
                let current = SettingsApi::snapshot::<OnboardingSettings>()?;
                if usize::from(page)
                    >= neuromita::onboarding::guide::count(current.value.guide_level)
                {
                    return Err(SettingsError::Validation(format!(
                        "Guide page {page} is outside selected level"
                    )));
                }
                SettingsApi::edit::<OnboardingSettings>(|state| self.edit_onboarding(state))?;
            }
            Self::Level(_) | Self::Complete => {
                SettingsApi::edit::<OnboardingSettings>(|state| self.edit_onboarding(state))?;
            }
            Self::Language(language) => {
                SettingsApi::edit::<InterfaceSettings>(|state| state.language = language)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatOrigin {
    pub character: CharacterId,
    pub selection_revision: u64,
}

pub enum Command {
    SelectSpeechModel(
        neuromita::ai_runtime::AiService,
        neuromita::settings::SpeechModelReference,
    ),
    RefreshReleases,
    MemorySelect(CharacterId),
    MemoryEdit(
        uuid::Uuid,
        neuromita::character::CharacterMemorySnapshot,
        Vec<neuromita::memory::MemoryCommand>,
    ),
    MemoryRecall(uuid::Uuid, CharacterId, neuromita::memory::MemoryId, String),
    GraphPreview(
        uuid::Uuid,
        CharacterId,
        neuromita::memory::MemoryId,
        neuromita::knowledge::maintenance::GraphMaintenanceMode,
    ),
    GraphApply(uuid::Uuid, CharacterId),
    GraphAbort(uuid::Uuid),
    Preview(ChatOrigin, ConversationChange),
    Confirm(ChatOrigin),
    Abort(ChatOrigin),
    Diagnostics(ChatOrigin),
    NewPreset,
    UpdatePreset(PresetId, neuromita::generation::PresetConnectionEdit),
    DeletePreset(PresetId),
    MovePreset(PresetId, i32),
    ActivePreset(PresetId),
    AssignPreset(CharacterId, PresetSlot, PresetSelection),
    AssignAllPresets(CharacterId, PresetSelection),
    SelectPromptSet(CharacterId, String),
    DebugSelect(CharacterId),
    SaveCharacterProfile(CharacterId, u64, neuromita::settings::CharacterSettings),
    RetryCharacterProfileSave(CharacterId),
    RefreshPromptCatalog,
    OpenPromptFolder(CharacterId),
    VoiceInLauncher(bool),
    VoiceInGame(bool),
    VoiceName(String),
    SaveTts(TtsPreferences),
    SaveCharacterVoice(Option<CharacterId>, String),
    SaveAsr(AsrPreferences, Option<String>),
    SelectAsrRecognizer(String),
    RefreshSpeechInventory,
    RefreshNativeModels,
    SelectNativeRevision {
        model_id: String,
        manifest_sha256: String,
        backend: String,
    },
    ActivateNativeModel {
        model_id: String,
        service: String,
        revision: String,
    },
    StopNativeModel {
        service: String,
        model_id: String,
        revision: String,
    },
    InstallNativePackage {
        model_id: String,
        manifest_sha256: String,
        backend: String,
    },
    CancelNativePackageInstall,
    PreviewAiHubInstall {
        category: String,
        id: String,
        token: i32,
    },
    ConfirmAiHubInstall {
        category: String,
        id: String,
        token: i32,
    },
    RefreshHardware,
    SectionVisibility(neuromita::settings::SettingsArea, bool),
    HidePrivate(bool),
    StructuredDisplay(neuromita::settings::StructuredDisplayMode),
    ChangeGuide {
        request_id: u64,
        change: GuideChange,
    },
    SetToolOption(ToolOption, bool),
    SetToolDepth(u8),
    SetMemoryDedupEnabled(bool),
    SetMemoryDedupThreshold(u16),
    Select(ChatOrigin),
    Send(ChatOrigin, String),
    AttachFile(ChatOrigin, String, String),
    Cancel(ChatOrigin, RequestId),
    Retry(ChatOrigin, RequestId),
    Refresh,
    LoadOlder(
        ChatOrigin,
        u64,
        neuromita::timeline::ContextAnchor,
        neuromita::timeline::selection::TimelineSelection,
        usize,
    ),
    Volume(u8),
    SpeechSessionEnabled(bool),
    ChatSpeechToggle(ChatOrigin, bool),
    SpeechPushToTalk(bool),
    MicrophoneTest(bool, String, String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphPreviewReport {
    pub count: usize,
    pub relations: Vec<neuromita::memory::ProvenancedRelation>,
}

impl GraphPreviewReport {
    fn from_plan(plan: &neuromita::knowledge::maintenance::GraphMaintenancePreview) -> Self {
        let relations = plan.inspected_relations();
        Self {
            count: relations.len(),
            relations,
        }
    }
}

pub enum Update {
    RuntimeStatus(RuntimeSnapshot),
    SpeechSessionStatus(SpeechSessionSnapshot),
    MicrophoneTestStatus(MonitorSnapshot),
    RecognizedText {
        origin: ChatOrigin,
        text: String,
        auto_send_after: Option<Duration>,
        merge_with_draft: bool,
    },
    Hardware(neuromita::presentation::hardware::HardwareView),
    CharacterCatalog(Vec<neuromita::character::CharacterDefinition>),
    SpeechInventory(Box<SpeechInventory>, Vec<AudioInputDevice>),
    NativeModels(Box<NativeWorkflowSnapshot>),
    NativeReleaseCatalog(Vec<NativeAuthorizedRelease>, NativeRevisionOverview),
    NativeError(String),
    NativeDownloadStatus {
        summary: String,
        percent: i32,
        active: bool,
    },
    AiHubInstallOperation {
        operation_id: String,
        category: String,
        id: String,
        stage: String,
        summary: String,
    },
    AiHubInstallPreview {
        category: String,
        id: String,
        token: i32,
        summary: String,
        error: String,
        stage: String,
        can_install: bool,
        operation_id: String,
    },
    Releases(Arc<neuromita::presentation::ReleasesSnapshot>),
    ReleaseError(String),
    Memory(Box<neuromita::presentation::MemoryListPage>),
    MemoryError(String),
    MemorySaved(uuid::Uuid),
    MemoryEditError(uuid::Uuid, String),
    MemoryRecallResult(uuid::Uuid, Result<Option<String>, String>),
    GraphCapability(bool),
    GraphPreviewResult(uuid::Uuid, Result<GraphPreviewReport, String>),
    GraphApplyResult(uuid::Uuid, Result<(), String>),
    Confirmation(ChatOrigin, Option<(ConversationChange, usize)>),
    Diagnostics(ChatOrigin, String),
    ConversationError(ChatOrigin, String),
    Connections(std::sync::Arc<neuromita::server::ServerSnapshot>),
    Administration(Box<super::settings::AdministrationSnapshot>),
    CharacterDebug {
        character: CharacterId,
        fields: Vec<neuromita::presentation::CharacterDebugField>,
        error: Option<String>,
    },
    PresetCreated(PresetId, PresetRecord),
    AdminSaved,
    GuidePersisted {
        request_id: u64,
        change: GuideChange,
    },
    GuideFailed {
        request_id: u64,
        error: String,
    },
    PresetSaved(PresetId),
    AdminError(String),
    CharacterProfileError(CharacterId, String),
    Stopped,
    Connecting,
    Connected(String),
    Snapshot(ChatOrigin, Box<ConversationSnapshot>),
    OlderPage(
        ChatOrigin,
        u64,
        Result<neuromita::presentation::ConversationHistoryPage, String>,
    ),
    Sent(ChatOrigin, String),
    FileAttached(ChatOrigin, String, String),
    Error(String),
    ChatError(ChatOrigin, String),
}

pub struct DesktopRuntime {
    pub commands: mpsc::Sender<Command>,
    stop: CancellationToken,
    thread: Option<thread::JoinHandle<()>>,
}

impl DesktopRuntime {
    pub fn spawn(
        directory: PathBuf,
        model: Option<String>,
        publish: impl Fn(Update) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (commands, receive) = mpsc::channel(32);
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let thread = thread::Builder::new()
            .name("neuromita-desktop".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        runtime.block_on(run(directory, model, receive, stopped, publish))
                    }
                    Err(error) => publish(Update::Error(error.to_string())),
                }
            })?;
        Ok(Self {
            commands,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for DesktopRuntime {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn find_binary(directory: &std::path::Path, variable: &str, filename: &str) -> Option<PathBuf> {
    [
        std::env::var_os(variable).map(PathBuf::from),
        Some(directory.join("AIData/bin").join(filename)),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.is_absolute() && path.is_file())
}

struct DesktopTranscriptSink {
    send: mpsc::UnboundedSender<(ChatOrigin, Transcript)>,
    origin: ChatOrigin,
}

fn speech_conversation(
    send: &mpsc::UnboundedSender<(ChatOrigin, Transcript)>,
    origin: ChatOrigin,
) -> Arc<dyn SpeechConversation> {
    Arc::new(DesktopTranscriptSink {
        send: send.clone(),
        origin,
    })
}

impl SpeechConversation for DesktopTranscriptSink {
    fn reply<'a>(
        &'a self,
        ticket: &'a neuromita::audio::ticket::SpeechTicket,
        transcript: &'a Transcript,
    ) -> SpeechFuture<'a, Option<neuromita::voice::AudioArtifact>> {
        Box::pin(async move {
            if let Some(result) = ticket.admit(|| self.send.send((self.origin, transcript.clone())))
            {
                result.map_err(|_| SpeechSessionError::DispatchClosed)?;
            }
            Ok(None)
        })
    }
}

fn dispatch_policy(preferences: &AsrPreferences) -> Option<Duration> {
    preferences.instant_send.then(|| {
        if preferences.delayed_send {
            Duration::from_millis(u64::from(preferences.send_delay_ms))
        } else {
            Duration::ZERO
        }
    })
}

async fn enable_speech_session(
    app: &ApplicationLifecycle,
    directory: &std::path::Path,
    conversation: Arc<dyn SpeechConversation>,
) -> Result<(), String> {
    let administration = app.presenter();
    let runtime = administration.ai_runtime.refresh_status().await;
    if runtime.service(neuromita::ai_runtime::AiService::Asr).phase != RuntimePhase::ModelReady {
        return Err("ASR модель не инициализирована; выберите и активируйте её в AI Engine".into());
    }
    let settings = administration
        .settings
        .snapshot()
        .map_err(|e| e.to_string())?;
    let preferences = &settings.values.speech.asr;
    let devices = administration
        .audio_input
        .devices()
        .await
        .map_err(|error| error.to_string())?;
    if !devices.iter().any(|device| {
        device.id == preferences.device_id && device.backend == preferences.audio_backend
    }) {
        return Err("Выбранное устройство ввода не найдено или не соответствует backend".into());
    }
    let ffmpeg = find_binary(
        directory,
        "NEUROMITA_FFMPEG",
        if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        },
    )
    .unwrap_or_default();
    let ffplay = find_binary(
        directory,
        "NEUROMITA_FFPLAY",
        if cfg!(windows) {
            "ffplay.exe"
        } else {
            "ffplay"
        },
    )
    .unwrap_or_default();
    let config = SpeechCaptureConfig::from_preferences(preferences, ffmpeg, ffplay)
        .map_err(|e| e.to_string())?;
    app.start_speech_capture_routed(config, conversation)
        .await
        .map_err(|e| e.to_string())
}

async fn start(
    directory: &std::path::Path,
    model: Option<&str>,
) -> Result<ApplicationLifecycle, String> {
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(GeminiProvider::new().map_err(|e| e.to_string())?);
    let generator = ResponseGenerator::new([
        provider,
        Arc::new(neuromita::generation::openai::OpenAiProvider::new().map_err(|e| e.to_string())?)
            as Arc<dyn GenerationProvider>,
    ])
    .map_err(|e| e.to_string())?;
    let limits = RuntimeLimits {
        request_queue: NonZeroUsize::new(16).unwrap(),
        discovery_buffer: NonZeroUsize::new(128).unwrap(),
        character_commands: NonZeroUsize::new(32).unwrap(),
        presenter_updates: NonZeroUsize::new(128).unwrap(),
        retained_finished: NonZeroUsize::new(100).unwrap(),
    };
    let bind = "127.0.0.1:0".parse().unwrap();
    let config = match model {
        Some(model) => {
            let key = std::env::var("GEMINI_API_KEY").map_err(|_| {
                "GEMINI_API_KEY is required for the explicitly selected startup model".to_owned()
            })?;
            let generation = GenerationSettings::new(0.7, 3000, Duration::from_secs(60))
                .map_err(|e| e.to_string())?;
            let template =
                GeminiTemplate::google(model, &key, generation).map_err(|e| e.to_string())?;
            ApplicationConfig::persistent(
                GenerationPreset::new(ProviderTemplate::Gemini(template)),
                generator,
                limits,
                bind,
                directory,
            )
        }
        None => ApplicationConfig::persistent_unconfigured(generator, limits, bind, directory),
    }
    .map_err(|e| e.to_string())?
    .with_installed_speech_runtime(
        neuromita::ai_runtime::installed_speech::InstalledSpeechEnvironment::for_desktop(
            directory.to_path_buf(),
        ),
    )
    .with_ai_hub_install_root(directory.to_path_buf());
    let root = std::env::var_os("NEUROMITA_PROMPTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| directory.join("Prompts"));
    let mut config = config.with_prompt_catalog(neuromita::prompts::PromptCatalog::new(root));
    config.game_link = Some(neuromita::game_link::GameLinkConfig::new(
        "127.0.0.1:12345"
            .parse()
            .expect("valid Unity loopback endpoint"),
    ));
    ApplicationLifecycle::start(config)
        .await
        .map_err(|e| e.to_string())
}

fn summarize_ai_hub_review(review: &InstallPlanReview) -> String {
    let mut lines = vec![
        format!(
            "Python: {} · SDK и источник проверены",
            review.identity.python_version
        ),
        format!("Backend: {}", review.backend_options.join(", ")),
        format!(
            "Пакетов: {} · требований: {}",
            review.packages.len(),
            review.requirements.len()
        ),
        format!(
            "Файлов моделей: обязательных {}, дополнительных {}",
            review.required_assets.len(),
            review.optional_assets.len()
        ),
        format!(
            "Действий с отдельным подтверждением: {} · hooks: {}",
            review.actions.len(),
            review.asset_hooks.len()
        ),
        format!(
            "Build-stage: {} (без автоматического исполнения)",
            review.build_stages.len()
        ),
        format!("Отпечаток плана: {}", &review.fingerprint[..16]),
        "План проверен, но установка ещё не разрешена и не запускалась.".to_owned(),
    ];
    if !review.required_backends.is_empty() {
        lines.insert(
            2,
            format!(
                "Дополнительные backend: {}",
                review.required_backends.join(", ")
            ),
        );
    }
    lines.join("\n")
}

async fn run(
    directory: PathBuf,
    initial_model: Option<String>,
    mut receive: mpsc::Receiver<Command>,
    stop: CancellationToken,
    publish: impl Fn(Update) + Send + Sync,
) {
    publish(Update::Connecting);
    let catalog = SpeechModelCatalog::for_desktop(directory.clone());
    let app = match start(&directory, initial_model.as_deref()).await {
        Ok(app) => app,
        Err(error) => {
            publish(Update::Error(error));
            publish(Update::Stopped);
            return;
        }
    };
    publish(Update::Connected(app.server().address().to_string()));
    publish(Update::GraphCapability(
        neuromita::api::GraphMaintenanceApi::reindex_available().unwrap_or(false),
    ));
    let devices = match app.presenter().audio_input.devices().await {
        Ok(devices) => devices,
        Err(error) => {
            publish(Update::AdminError(error.to_string()));
            Vec::new()
        }
    };
    publish(Update::SpeechInventory(
        Box::new(catalog.snapshot().await),
        devices,
    ));
    if let Some(problem) = app.speech_startup_warning() {
        publish(Update::AdminError(format!("Speech runtime: {problem}")));
    }
    publish(hardware_snapshot().await);
    publish(Update::CharacterCatalog(
        app.presenter().characters.catalog(),
    ));
    if let Err(error) = observe(&app, &directory, &catalog, &mut receive, &stop, &publish).await {
        publish(Update::Error(error));
    }
    if let Err(error) = app.shutdown().await {
        publish(Update::Error(error.to_string()));
    }
    publish(Update::GraphCapability(false));
    publish(Update::Stopped);
}

pub fn native_download_configured() -> bool {
    neuromita::ai_runtime::native_service::native_download_configured()
}

async fn publish_native_release_catalog(native: &AiHubNativeService, publish: &impl Fn(Update)) {
    let overview = match native.snapshot().await {
        Ok(snapshot) => snapshot.inventory,
        Err(error) => {
            publish(Update::NativeError(format!(
                "Каталог установленных ревизий отклонён: {error}"
            )));
            NativeRevisionOverview {
                revisions: vec![],
                selections: vec![],
            }
        }
    };
    match native.authorized_releases() {
        Ok(releases) => publish(Update::NativeReleaseCatalog(releases, overview)),
        Err(error) => {
            publish(Update::NativeReleaseCatalog(vec![], overview));
            publish(Update::NativeError(format!(
                "Каталог подписанных релизов отклонён: {error}"
            )));
        }
    }
}

async fn publish_native(native: &AiHubNativeService, publish: &impl Fn(Update)) {
    match native.snapshot().await {
        Ok(snapshot) => publish(Update::NativeModels(Box::new(snapshot))),
        Err(error) => publish(Update::NativeError(format!(
            "Не удалось прочитать каталог нативных ревизий: {error}"
        ))),
    }
}

fn native_backend(value: &str) -> Option<NativeBackend> {
    match value {
        "cpu" => Some(NativeBackend::Cpu),
        "cuda" => Some(NativeBackend::Cuda),
        "wgpu" => Some(NativeBackend::Wgpu),
        "onnx" => Some(NativeBackend::Onnx),
        _ => None,
    }
}

async fn observe(
    app: &ApplicationLifecycle,
    directory: &std::path::Path,
    catalog: &SpeechModelCatalog,
    receive: &mut mpsc::Receiver<Command>,
    stop: &CancellationToken,
    publish: &impl Fn(Update),
) -> Result<(), String> {
    let administration = app.presenter();
    let mut releases = administration.releases.observe();
    let presenter = &administration.conversation;
    let mut connections = administration.connections.observe();
    let mut memories = MemoryWatch::start(administration, CharacterId::Crazy)?;
    let (recall_updates, mut recall_results) = mpsc::unbounded_channel();
    let mut graph_pending: Option<(
        uuid::Uuid,
        CharacterId,
        neuromita::knowledge::maintenance::GraphMaintenancePreview,
    )> = None;
    let mut pending: Option<(ChatOrigin, ConversationChangePreview)> = None;
    let mut character = CharacterId::Crazy;
    let mut selection_revision = 0;
    let mut speech_owner: Option<ChatOrigin> = None;
    let mut debug_character = CharacterId::Crazy;
    let mut requests = presenter.observe_requests();
    let mut settings = presenter.observe_settings().map_err(|e| e.to_string())?;
    let mut presets = presenter.observe_presets();
    let mut selection = presenter.observe_selection().map_err(|e| e.to_string())?;
    let mut runtime_status = administration.ai_runtime.observe();
    let mut speech_status = administration.speech.observe();
    let mut microphone_test = administration.audio_input.observe_test();
    let install_owner = app
        .ai_hub_install()
        .expect("desktop configured Core AI Hub installer");
    let mut install_updates = install_owner
        .take_events()
        .expect("only one desktop consumes Core AI Hub events");
    let (native_download_events, mut native_download_updates) =
        mpsc::unbounded_channel::<NativeDownloadEvent>();
    let mut active_native_download: Option<uuid::Uuid> = None;
    let (transcripts, mut transcript_updates) = mpsc::unbounded_channel();
    let mut liveness = tokio::time::interval(Duration::from_secs(5));
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let native_owner = app
        .ai_hub_native()
        .expect("desktop configured Core native owner");
    let initial_native = native_owner.refresh().await;
    if initial_native.is_err() {
        publish(Update::NativeError(
            "Ошибка при обнаружении нативных моделей".into(),
        ));
    }
    publish_native(native_owner, publish).await;
    publish_native_release_catalog(native_owner, publish).await;
    let mut refresh_views = true;
    loop {
        if refresh_views {
            publish(Update::Releases(releases.borrow_and_update().clone()));
            publish(Update::Connections(connections.borrow_and_update().clone()));
            requests.borrow_and_update();
            settings.borrow_and_update();
            presets.borrow_and_update();
            selection.borrow_and_update();
            match administration.settings.snapshot() {
                Ok(settings) => publish(Update::Administration(Box::new(
                    super::settings::AdministrationSnapshot {
                        settings,
                        presets: administration.presets.snapshot(),
                        prompt_sets: administration
                            .prompts
                            .as_ref()
                            .map(|catalog| {
                                CharacterId::ALL
                                    .into_iter()
                                    .flat_map(|id| catalog.list(id).unwrap_or_default())
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                ))),
                Err(error) => publish(Update::AdminError(error.to_string())),
            }
            publish(character_debug_snapshot(administration, debug_character).await);
            match presenter.snapshot(character).await {
                Ok(snapshot) => publish(Update::Snapshot(
                    ChatOrigin {
                        character,
                        selection_revision,
                    },
                    Box::new(snapshot),
                )),
                Err(error) => publish(Update::ChatError(
                    ChatOrigin {
                        character,
                        selection_revision,
                    },
                    error.to_string(),
                )),
            }
            publish(Update::RuntimeStatus(administration.ai_runtime.snapshot()));
            publish(Update::SpeechSessionStatus(
                administration.speech.snapshot(),
            ));
        }
        refresh_views = true;
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            command = receive.recv() => match command {
                Some(command @ Command::SaveCharacterProfile(character, ..)) |
                Some(command @ Command::RetryCharacterProfileSave(character)) => {
                    match super::settings::execute(administration, command) {
                        Ok(()) => publish(Update::AdminSaved),
                        Err(error) => publish(Update::CharacterProfileError(character, error)),
                    }
                },
                Some(command @ Command::UpdatePreset(id, _)) => {
                    match super::settings::execute(administration, command) {
                        Ok(()) => publish(Update::PresetSaved(id)),
                        Err(error) => publish(Update::AdminError(error)),
                    }
                },
                Some(Command::ChangeGuide { request_id, change }) => {
                    let result = change.apply();
                    let result = match result {
                        Ok(()) => SettingsApi::flush().await.map_err(|error| error.to_string()),
                        Err(error) => Err(error.to_string()),
                    };
                    match result {
                        Ok(()) => publish(Update::GuidePersisted { request_id, change }),
                        Err(error) => publish(Update::GuideFailed { request_id, error }),
                    }
                },
                Some(command @ (Command::DeletePreset(_) | Command::MovePreset(..) | Command::ActivePreset(_) | Command::AssignPreset(..) | Command::AssignAllPresets(..) | Command::SelectPromptSet(..) | Command::VoiceInLauncher(_) | Command::VoiceInGame(_) | Command::VoiceName(_) | Command::SectionVisibility(..) | Command::HidePrivate(_) | Command::StructuredDisplay(_) | Command::SetToolOption(..) | Command::SetToolDepth(_) | Command::SetMemoryDedupEnabled(_) | Command::SetMemoryDedupThreshold(_) | Command::SaveTts(_) | Command::SaveAsr(..) | Command::SaveCharacterVoice(..) | Command::SelectAsrRecognizer(_))) => {
                    match super::settings::execute(administration, command) {
                        Ok(()) => publish(Update::AdminSaved),
                        Err(error) => publish(Update::AdminError(error)),
                    }
                },
                Some(Command::SelectSpeechModel(service, model)) => {
                    let result = match service {
                        neuromita::ai_runtime::AiService::Tts => administration.settings.tts.model.set(Some(model)),
                        neuromita::ai_runtime::AiService::Asr => administration.settings.asr.model.set(Some(model)),
                        _ => Err(neuromita::settings::SettingsError::Validation("not a speech service".into())),
                    };
                    match result { Ok(()) => publish(Update::AdminSaved), Err(error) => publish(Update::AdminError(error.to_string())) }
                },
                Some(Command::NewPreset) => match administration.presets.create_unconfigured() {
                    Ok((id, record)) => publish(Update::PresetCreated(id, record)),
                    Err(error) => publish(Update::AdminError(error.to_string())),
                },
                Some(Command::SpeechSessionEnabled(enabled)) => {
                    let result = if enabled {
                        enable_speech_session(app, directory, speech_conversation(&transcripts, ChatOrigin {
                            character, selection_revision,
                        })).await
                    } else {
                        app.stop_speech_capture().await;
                        Ok(())
                    };
                    if result.is_ok() {
                        speech_owner = enabled.then_some(ChatOrigin { character, selection_revision });
                    }
                    if let Err(error) = result { publish(Update::AdminError(error)); }
                    publish(Update::SpeechSessionStatus(administration.speech.snapshot()));
                },
                Some(Command::ChatSpeechToggle(origin, enabled)) => {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        continue;
                    }
                    let result = if enabled {
                        match enable_speech_session(
                            app,
                            directory,
                            speech_conversation(&transcripts, origin),
                        ).await {
                            Ok(()) => {
                                let mode = administration.settings.asr.preferences.get().input_mode;
                                if matches!(mode.as_str(), "radio" | "push_to_talk") {
                                    match app.press_speech_button(true).await {
                                        Ok(()) => Ok(()),
                                        Err(error) => {
                                            app.stop_speech_capture().await;
                                            Err(error.to_string())
                                        }
                                    }
                                } else {
                                    Ok(())
                                }
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        let _ = app.press_speech_button(false).await;
                        app.stop_speech_capture().await;
                        Ok(())
                    };
                    if result.is_ok() {
                        speech_owner = enabled.then_some(origin);
                    }
                    if let Err(error) = result { publish(Update::ChatError(origin, error)); }
                    publish(Update::SpeechSessionStatus(administration.speech.snapshot()));
                },
                Some(Command::SpeechPushToTalk(pressed)) => {
                    if let Err(error) = app.press_speech_button(pressed).await && pressed {
                        publish(Update::AdminError(error.to_string()));
                    }
                },
                Some(Command::MicrophoneTest(enabled, backend, device)) => {
                    let result = if enabled {
                        administration.audio_input.start_test(&backend, &device).await
                    } else {
                        administration.audio_input.stop_test().await
                    };
                    if let Err(error) = result { publish(Update::AdminError(error.to_string())); }
                },
                Some(Command::RefreshNativeModels) => {
                    if let Err(error) = native_owner.refresh().await {
                        publish(Update::NativeError(format!("Ошибка при обнаружении нативных моделей: {error}")));
                    }
                    publish_native(native_owner, publish).await;
                    publish_native_release_catalog(native_owner, publish).await;
                    refresh_views = false;
                },
                Some(Command::SelectNativeRevision { model_id, manifest_sha256, backend }) => {
                    let result = match native_backend(&backend) {
                        Some(backend) => native_owner.select(&model_id, &manifest_sha256, backend).await,
                        None => Err("Нативная ревизия или backend недоступны".into()),
                    };
                    publish_native(native_owner, publish).await;
                    if result.is_err() {
                        publish(Update::NativeError("Выбор отклонён: необходимы подтверждённая подпись издателя и целостный совместимый пакет".into()));
                    }
                    refresh_views = false;
                },
                Some(Command::ActivateNativeModel { model_id, service, revision }) => {
                    let result = match service.as_str() {
                        "tts" => native_owner.activate_exact_for_service(neuromita::ai_runtime::native_speech::NativeSpeechKind::Tts, &model_id, &revision).await,
                        "asr" => native_owner.activate_exact_for_service(neuromita::ai_runtime::native_speech::NativeSpeechKind::Asr, &model_id, &revision).await,
                        _ => Err("Модель не объявляет типизированный сервис".into()),
                    };
                    publish_native(native_owner, publish).await;
                    if result.is_err() {
                        publish(Update::NativeError("Запуск отклонён: подпись издателя, сервис, ревизия, файлы или ответ worker не подтверждены".into()));
                    }
                    refresh_views = false;
                },
                Some(Command::StopNativeModel { service, model_id, revision }) => {
                    let result = match service.as_str() {
                        "tts" => native_owner.deactivate_exact_for_service(neuromita::ai_runtime::native_speech::NativeSpeechKind::Tts, &model_id, &revision).await,
                        "asr" => native_owner.deactivate_exact_for_service(neuromita::ai_runtime::native_speech::NativeSpeechKind::Asr, &model_id, &revision).await,
                        _ => Err("Невозможно остановить сервис с неизвестным типом".into()),
                    };
                    publish_native(native_owner, publish).await;
                    if result.is_err() {
                        publish(Update::NativeError("Завершение нативного worker не подтверждено".into()));
                    }
                    refresh_views = false;
                },
                Some(Command::InstallNativePackage { model_id, manifest_sha256, backend }) => {
                    if active_native_download.is_some() {
                        publish(Update::NativeDownloadStatus {
                            summary: "Установка уже выполняется; сначала завершите или отмените её".into(),
                            percent: -1, active: true,
                        });
                    } else if let Some(backend) = native_backend(&backend) {
                        match native_owner.start_download(
                            NativeDownloadSelection { model_id, manifest_sha256, backend },
                            native_download_events.clone(),
                        ).await {
                            Ok(operation) => {
                                active_native_download = Some(operation);
                                publish(Update::NativeDownloadStatus {
                                    summary: "Проверка подписи пройдена. Подключение к доверенному HTTPS-источнику…".into(),
                                    percent: -1, active: true,
                                });
                            },
                            Err(error) => publish(Update::NativeDownloadStatus {
                                summary: format!("Установка отклонена: {error}"),
                                percent: -1, active: false,
                            }),
                        }
                    } else {
                        publish(Update::NativeDownloadStatus {
                            summary: "Неизвестный native backend".into(), percent: -1, active: false,
                        });
                    }
                    refresh_views = false;
                },
                Some(Command::CancelNativePackageInstall) => {
                    if native_owner.cancel_download().await {
                        publish(Update::NativeDownloadStatus {
                            summary: "Отмена запрошена. Начатая атомарная публикация может завершиться целиком".into(),
                            percent: -1, active: true,
                        });
                    }
                    refresh_views = false;
                },
                Some(Command::RefreshPromptCatalog) => {},
                Some(Command::DebugSelect(id)) => { debug_character = id; },
                Some(Command::OpenPromptFolder(character)) => {
                    if let Some(catalog) = &administration.prompts {
                        let result = (|| -> Result<(), String> {
                            let settings = administration.settings.snapshot().map_err(|e| e.to_string())?;
                            let selected = settings.values.characters.for_character(character).prompt_set.as_deref();
                            let character_root = catalog.root().join(character.to_string());
                            let folder = match selected {
                                Some(name) => {
                                    let name = catalog.resolve_set(character, Some(name)).map_err(|e| e.to_string())?;
                                    character_root.join(name)
                                }
                                None => character_root,
                            };
                            std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
                            let folder = folder.canonicalize().map_err(|e| e.to_string())?;
                            let command = if cfg!(windows) { "explorer.exe" } else { "xdg-open" };
                            std::process::Command::new(command).arg(folder).spawn().map_err(|e| e.to_string())?;
                            Ok(())
                        })();
                        if let Err(error) = result { publish(Update::AdminError(error)); }
                    }
                },
                Some(Command::RefreshHardware) => publish(hardware_snapshot().await),
                Some(Command::PreviewAiHubInstall { category, id, token }) => {
                    install_owner.preview(category, id, token).await;
                    refresh_views = false;
                },
                Some(Command::ConfirmAiHubInstall { category, id, token }) => {
                    match install_owner.confirm(category.clone(), id.clone(), token).await {
                        Ok((operation, action)) => {
                            let (operation_summary, preview_summary) = match &action {
                                InstallAction::Overlay => (
                                    "Публикация проверенного plugin overlay",
                                    "Установка plugin overlay выполняется. Worker не запускается.",
                                ),
                                InstallAction::Wheel { .. } => (
                                    "Установка одного проверенного wheel-stage",
                                    "Устанавливается один wheel-stage. Модель и worker не активируются.",
                                ),
                            };
                            publish(Update::AiHubInstallOperation {
                                operation_id: operation.to_string(),
                                category: category.clone(), id: id.clone(),
                                stage: "installing".into(), summary: operation_summary.into(),
                            });
                            publish(Update::AiHubInstallPreview {
                                category, id, token,
                                summary: preview_summary.into(), error: String::new(),
                                stage: "installing".into(), can_install: false,
                                operation_id: operation.to_string(),
                            });
                        },
                        Err(error) => publish(Update::AiHubInstallPreview {
                            category, id, token, summary: String::new(), error: error.into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new(),
                        }),
                    }
                    refresh_views = false;
                },
                Some(Command::RefreshSpeechInventory) => {
                    install_owner.invalidate().await;

                    let devices = match administration.audio_input.devices().await {
                        Ok(devices) => devices,
                        Err(error) => {
                            publish(Update::AdminError(error.to_string()));
                            Vec::new()
                        }
                    };
                    publish(Update::SpeechInventory(Box::new(catalog.snapshot().await), devices));
                },
                Some(Command::RefreshReleases) => { if let Err(error) = administration.releases.refresh() { publish(Update::ReleaseError(error.to_string())); } },
                None => return Ok(()),
                Some(Command::MemorySelect(id)) => {
                    graph_pending = None;
                    memories = MemoryWatch::start(administration, id)?;
                },
                Some(Command::GraphPreview(request_id, selected, memory_id, mode)) => {
                    graph_pending = None;
                    let result = if mode == neuromita::knowledge::maintenance::GraphMaintenanceMode::Reindex
                        && !neuromita::api::GraphMaintenanceApi::reindex_available().unwrap_or(false)
                    {
                        Err("Graph Reindex backend is unavailable".to_owned())
                    } else {
                        neuromita::api::GraphMaintenanceApi::preview(
                            selected,
                            &[memory_id],
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX),
                            mode,
                        ).await.map_err(|error| error.to_string())
                    };
                    match result {
                        Ok(plan) => {
                            let report = GraphPreviewReport::from_plan(&plan);
                            graph_pending = Some((request_id, selected, plan));
                            publish(Update::GraphPreviewResult(request_id, Ok(report)));
                        }
                        Err(error) => publish(Update::GraphPreviewResult(request_id, Err(error))),
                    }
                },
                Some(Command::GraphApply(request_id, selected)) => {
                    let result = match graph_pending.take() {
                        Some((id, owner, plan)) if id == request_id && owner == selected => {
                            neuromita::api::GraphMaintenanceApi::apply_confirmed(
                                selected, plan, true, stop,
                            ).await.map(|_| ()).map_err(|error| error.to_string())
                        }
                        other => {
                            graph_pending = other;
                            Err("Graph preview is missing or belongs to another selection".to_owned())
                        }
                    };
                    publish(Update::GraphApplyResult(request_id, result));
                },
                Some(Command::GraphAbort(request_id)) => {
                    if graph_pending.as_ref().is_some_and(|(id, _, _)| *id == request_id) {
                        graph_pending = None;
                    }
                },
                Some(Command::MemoryEdit(edit_id, expected, commands)) => {
                    match administration.characters.edit_memories(expected.character(), expected, commands).await {
                        Ok(_) => publish(Update::MemorySaved(edit_id)), Err(error) => publish(Update::MemoryEditError(edit_id, error.to_string())),
                    }
                },
                Some(Command::MemoryRecall(request_id, character, memory_id, query)) => {
                    let sender = recall_updates.clone();
                    let cancelled = stop.clone();
                    tokio::spawn(async move {
                        let result = tokio::select! {
                            _ = cancelled.cancelled() => return,
                            result = neuromita::api::HistoricalRecallApi::inspect(
                                character,
                                neuromita::knowledge::forgotten_recall::RecallSelector::Ids(vec![memory_id]),
                                &query,
                            ) => result.map_err(|error| error.to_string()),
                        };
                        let _ = sender.send(Update::MemoryRecallResult(request_id, result));
                    });
                },
                Some(Command::Preview(origin, operation)) => {
                    if chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        pending = None;
                        match presenter.preview_change(origin.character, operation).await {
                            Ok(preview) => {
                                publish(Update::Confirmation(origin, Some((preview.operation(), preview.message_count()))));
                                pending = Some((origin, preview));
                            }
                            Err(error) => {
                                publish(Update::Confirmation(origin, None));
                                publish(Update::ConversationError(origin, error.to_string()));
                            },
                        }
                    }
                },
                Some(Command::Confirm(origin)) => {
                    if chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) && pending.as_ref().is_some_and(|(owner, _)| *owner == origin) {
                        if let Some((_, preview)) = pending.take()
                            && let Err(error) = presenter.apply_change(preview)
                        {
                            publish(Update::ConversationError(origin, error.to_string()));
                        }
                        publish(Update::Confirmation(origin, None));
                    }
                },
                Some(Command::Abort(origin)) => {
                    if pending.as_ref().is_some_and(|(owner, _)| *owner == origin) {
                        pending = None;
                        publish(Update::Confirmation(origin, None));
                    }
                },
                Some(Command::Diagnostics(origin)) => {
                    if chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        let model = administration.chat.snapshot();
                        let trace = model.requests.iter().rev().find(|r| r.metadata.character_id == origin.character).and_then(|r| administration.chat.trace(r.metadata.id));
                        let text = match trace {
                            Some(trace) => match trace.section_json(neuromita::request::DiagnosticSection::All) {
                                Ok(text) => text,
                                Err(error) => format!("Cannot export request diagnostics: {error}"),
                            },
                            None => "No retained request trace".into(),
                        };
                        publish(Update::Diagnostics(origin, text));
                    }
                },
                Some(Command::Select(origin)) => {
                    if speech_owner.is_some_and(|owner| owner != origin) {
                        app.stop_speech_capture().await;
                        speech_owner = None;
                        publish(Update::SpeechSessionStatus(administration.speech.snapshot()));
                    }
                    character = origin.character;
                    selection_revision = origin.selection_revision;
                    pending = None;
                    publish(Update::Confirmation(origin, None));
                },
                Some(Command::Refresh) => {},
                Some(Command::LoadOlder(origin, ticket, at, expected, offset)) => {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        continue;
                    }
                    let page = presenter.history_page(origin.character, at, expected, offset)
                        .await.map_err(|error| error.to_string());
                    publish(Update::OlderPage(origin, ticket, page));
                },
                Some(Command::Send(origin, text)) => {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        publish(Update::ChatError(origin, "Chat selection changed before send".into()));
                    } else {
                        match presenter.reply(origin.character, text.clone()) {
                            Ok(_) => publish(Update::Sent(origin, text)),
                            Err(error) => publish(Update::ChatError(origin, error.to_string())),
                        }
                    }
                },
                Some(Command::AttachFile(origin, text, source)) => {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        publish(Update::ChatError(origin, "Chat selection changed before attachment".into()));
                        continue;
                    }
                    let result = (|| -> Result<(), String> {
                        use neuromita::timeline::{
                            file_attachments::{compose_local_file_message, import_local_file},
                            storage::assets::TimelineAssetStore,
                        };
                        let selection = app.timeline_selection();
                        let assets = TimelineAssetStore::new(directory.join("TimelineAssets"), 8 * 1024 * 1024)
                            .map_err(|error| error.to_string())?;
                        let part = import_local_file(&assets, std::path::Path::new(&source))
                            .map_err(|error| error.to_string())?;
                        let message = compose_local_file_message(origin.character, text.clone(), part)
                            .map_err(|error| error.to_string())?;
                        app.publish_attached_conversation_message(selection, message, &assets)
                            .map_err(|error| error.to_string())?;
                        Ok(())
                    })();
                    match result {
                        Ok(()) => publish(Update::FileAttached(origin, text, source)),
                        Err(error) => publish(Update::ChatError(origin, error)),
                    }
                },
                Some(Command::Cancel(origin, id)) => {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        continue;
                    }
                    if !administration.chat.snapshot().requests.iter().any(|request| {
                        request.metadata.id == id && request.metadata.character_id == origin.character
                    }) {
                        publish(Update::ChatError(origin, "Request is not owned by the selected character".into()));
                        continue;
                    }
                    match presenter.cancel(id) {
                        Ok(CancelResult::Cancelled | CancelResult::Requested) => {},
                        Ok(CancelResult::TooLate | CancelResult::AlreadyFinished) =>
                            publish(Update::ChatError(origin, "The result has already been accepted; cancellation is too late".into())),
                        Err(error) => publish(Update::ChatError(origin, error.to_string())),
                    }
                },
                Some(Command::Retry(origin, id)) => {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        continue;
                    }
                    if let Err(error) = presenter.retry_failed(origin.character, id).await {
                        publish(Update::ChatError(origin, error.to_string()));
                    }
                },
                Some(Command::Volume(value)) => match Volume::new(value).and_then(|v| presenter.set_volume(v)) {
                    Ok(()) => {},
                    Err(error) => publish(Update::AdminError(error.to_string())),
                },
            },
            result = recall_results.recv() => {
                if let Some(result) = result { publish(result); }
            },
            installed = install_updates.recv() => {
                match installed {
                    Some(InstallEvent::Finished { operation, category, id, token, action, result }) => {
                        install_owner.join_finished(operation).await;
                        let (summary, error, stage): (String, String, String) = match result {
                            Ok(_) => match action {
                                InstallAction::Overlay => ("Plugin overlay установлен и проверен. Worker не запущен, model assets не установлены.".into(), String::new(), "installed".into()),
                                InstallAction::Wheel { .. } => ("Один wheel-stage установлен и проверен. Это не готовность backend, модели или worker.".into(), String::new(), "wheel-installed".into()),
                            },
                            Err(InstallPreviewError::ReviewChanged) => (String::new(), "Исходный план изменился после подтверждения — установка остановлена.".into(), "failed".into()),
                            Err(InstallPreviewError::UnsupportedInstall) => (String::new(), "Этот план требует дополнительного подтверждения backend или файлов модели.".into(), "failed".into()),
                            Err(_) => (String::new(), "Core не смог завершить установку. Подробности доступны в диагностике установщика.".into(), "failed".into()),
                        };
                        publish(Update::AiHubInstallOperation {
                            operation_id: operation.to_string(),
                            category: category.clone(), id: id.clone(),
                            stage: stage.clone(),
                            summary: if error.is_empty() { summary.clone() } else { error.clone() },
                        });
                        publish(Update::AiHubInstallPreview { category, id, token,
                            summary, error, stage, can_install: false, operation_id: operation.to_string() });
                    },
                    Some(InstallEvent::Preview { category, id, token, result, admitted }) => {
                        if !install_owner.is_current_preview(&category, &id, token).await {
                            continue;
                        }
                        let (summary, error, can_install, stage) = match result {
                            Ok(InstallActionReview { review, action }) => {
                                let mut summary = summarize_ai_hub_review(&review);
                                let review_stage = match action {
                                    Some(InstallAction::Overlay) if admitted => "review",
                                    Some(InstallAction::Wheel { stage_id, backend }) => {
                                        summary.push_str(&format!(
                                            "\nДоступен проверенный wheel-stage: {stage_id} · {backend}. Установится только этот пакет, не модель.",
                                        ));
                                        if admitted { "wheel-review" } else { "review" }
                                    },
                                    _ => {
                                        summary.push_str("\nИсполнение недоступно: нет единственного проверенного и локально доступного wheel-stage либо необходимы другие установочные этапы.");
                                        "review"
                                    },
                                };
                                (summary, String::new(), admitted, review_stage.to_owned())
                            },
                            Err(InstallPreviewError::InvalidSelection) => (String::new(),
                                "Модель недоступна или неоднозначна в актуальном каталоге".into(), false, "failed".into()),
                            Err(InstallPreviewError::Sdk(_)) => (String::new(),
                                "Для проверки необходимы установленные в UserData проверенный SDK и код плагина.".into(), false, "failed".into()),
                            Err(InstallPreviewError::PythonVersion | InstallPreviewError::Python(_)) => (String::new(),
                                "Managed CPython 3.12.x ещё не подготовлен либо не прошёл проверку версии.".into(), false, "failed".into()),
                            Err(InstallPreviewError::Plan(_)) => (String::new(),
                                "Установочный скрипт не предоставил допустимый план. Установка не запускалась.".into(), false, "failed".into()),
                            Err(InstallPreviewError::InvalidPlan(_)) => (String::new(),
                                "Полученный план не прошёл проверку зависимостей, backend или путей.".into(), false, "failed".into()),
                            Err(InstallPreviewError::ReviewChanged) => (String::new(),
                                "План или исходник изменился во время проверки.".into(), false, "failed".into()),
                            Err(InstallPreviewError::UnsupportedInstall) => (String::new(),
                                "Для этого плана пока не поддерживается установка.".into(), false, "failed".into()),
                            Err(InstallPreviewError::WheelPin | InstallPreviewError::WheelExecution) => (String::new(),
                                "Проверенный wheel-stage недоступен или не смог завершиться; установка модели не запускалась.".into(), false, "failed".into()),
                        };
                        publish(Update::AiHubInstallPreview { category, id, token, summary, error,
                            stage, can_install, operation_id: String::new() });
                    },
                    None => {},
                }
            },
            transcript = transcript_updates.recv() => {
                if let Some((origin, transcript)) = transcript {
                    if !chat_fence::accepts(
                        origin.character.index() as i32,
                        origin.selection_revision,
                        character.index() as i32,
                        selection_revision,
                    ) {
                        continue;
                    }
                    let text = transcript.text.trim();
                    if !text.is_empty() {
                        let preferences = administration.settings.asr.preferences.get();
                        publish(Update::RecognizedText {
                            origin,
                            text: text.to_owned(),
                            auto_send_after: dispatch_policy(&preferences),
                            merge_with_draft: preferences.merge_chat,
                        });
                    }
                }
            },
            update = memories.updates.recv(), if !memories.closed => { match update { Some(update) => publish(update), None => memories.closed = true } },
            result = speech_status.changed() => {
                result.map_err(|_| "Speech session observation closed")?;
                publish(Update::SpeechSessionStatus(speech_status.borrow_and_update().clone()));
            },
            result = microphone_test.changed() => {
                result.map_err(|_| "Microphone test observation closed")?;
                publish(Update::MicrophoneTestStatus(microphone_test.borrow_and_update().clone()));
                refresh_views = false;
            },
            result = releases.changed() => { result.map_err(|_| "Release observation closed")?; },
            result = connections.changed() => { result.map_err(|_| "Connection observation closed")?; },
            result = requests.changed() => {
                result.map_err(|_| "Request observation closed")?;
                let origin = ChatOrigin { character, selection_revision };
                match presenter.snapshot(character).await {
                    Ok(snapshot) => publish(Update::Snapshot(origin, Box::new(snapshot))),
                    Err(error) => publish(Update::ChatError(origin, error.to_string())),
                }
                refresh_views = false;
            },
            result = settings.changed() => { result.map_err(|_| "Settings observation closed")?; },
            result = presets.changed() => { result.map_err(|_| "Preset observation closed")?; },
            result = selection.changed() => { result.map_err(|_| "Timeline observation closed")?; },
            result = async {
                match runtime_status.as_mut() {
                    Some(receiver) => receiver.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if result.is_err() { runtime_status = None; }
            },
            event = native_download_updates.recv() => {
                if let Some(event) = event {
                    let matching = active_native_download == Some(event.id());
                    if matching {
                        match event {
                            NativeDownloadEvent::Progress { progress, .. } => {
                                let view = NativeDownloadPresenter::project(progress);
                                let summary = match view.phase {
                                    NativeTransferPhase::Manifest => "Загрузка подписанного manifest…".to_owned(),
                                    NativeTransferPhase::Downloading => format!(
                                        "Загружено {} / {} МиБ. Пакет пока не установлен",
                                        view.transferred_bytes / (1024 * 1024),
                                        view.planned_bytes.unwrap_or(0) / (1024 * 1024),
                                    ),
                                    NativeTransferPhase::Publishing => "Повторная проверка и атомарная публикация…".to_owned(),
                                };
                                publish(Update::NativeDownloadStatus {
                                    summary,
                                    percent: view.percent.map_or(-1, i32::from),
                                    active: true,
                                });
                            },
                            NativeDownloadEvent::Finished { result, .. } => {
                                if let Some(operation) = active_native_download.take() {
                                    native_owner.finish_download(operation).await;
                                }
                                match result {
                                    Ok(installed) => {
                                        if let Err(error) = native_owner.refresh().await {
                                            publish(Update::NativeError(error));
                                        }
                                        publish_native(native_owner, publish).await;
                                        publish_native_release_catalog(native_owner, publish).await;
                                        publish(Update::NativeDownloadStatus {
                                            summary: format!("Пакет {} опубликован (повторное использование: {})",
                                                installed.model_id, installed.reused_package),
                                            percent: 100,
                                            active: false,
                                        });
                                    },
                                    Err(error) => publish(Update::NativeDownloadStatus {
                                        summary: format!("Установка не выполнена: {error}"),
                                        percent: -1,
                                        active: false,
                                    }),
                                }
                            },
                        }
                    }
                }
                refresh_views = false;
            },
            _ = liveness.tick() => {
                let status = administration.ai_runtime.refresh_status().await;
                publish(Update::RuntimeStatus(status));
                let lost = native_owner.refresh_worker().await.is_err();
                publish_native(native_owner, publish).await;
                if lost {
                    publish(Update::NativeError("Связь с нативным worker прервана".into()));
                }
            },
        }
    }
}

async fn character_debug_snapshot(api: &GuiPresenter, character: CharacterId) -> Update {
    let result = async {
        let post_dsl = if let Some(catalog) = api.prompts.as_ref() {
            let settings = api.settings.snapshot().map_err(|error| error.to_string())?;
            let selected = settings
                .values
                .characters
                .for_character(character)
                .prompt_set
                .as_deref();
            if selected.is_none()
                && catalog
                    .list(character)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            {
                // Match request admission: without an explicit selection and
                // without any packs, use the built-in character debug fields.
                None
            } else {
                let name = catalog
                    .resolve_set(character, selected)
                    .map_err(|error| error.to_string())?;
                catalog
                    .load_post_rules(character, &name)
                    .map_err(|error| error.to_string())?
            }
        } else {
            None
        };
        api.characters
            .debug_display(character, post_dsl.as_ref())
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    match result {
        Ok(fields) => Update::CharacterDebug {
            character,
            fields,
            error: None,
        },
        Err(error) => Update::CharacterDebug {
            character,
            fields: Vec::new(),
            error: Some(error),
        },
    }
}

struct MemoryWatch {
    updates: mpsc::Receiver<Update>,
    task: tokio::task::JoinHandle<()>,
    closed: bool,
}
impl MemoryWatch {
    fn start(api: &GuiPresenter, character: CharacterId) -> Result<Self, String> {
        let mut observer = api
            .characters
            .observe_memories(character, Default::default())
            .map_err(|e| e.to_string())?;
        let (send, updates) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            loop {
                let (update, failed) = match observer.next().await {
                    Ok(page) => (Update::Memory(Box::new(page)), false),
                    Err(error) => (Update::MemoryError(error.to_string()), true),
                };
                if send.send(update).await.is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            updates,
            task,
            closed: false,
        })
    }
}
impl Drop for MemoryWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn hardware_snapshot() -> Update {
    let inventory =
        tokio::task::spawn_blocking(neuromita::ai_runtime::system_hardware::SystemHardware::detect)
            .await
            .unwrap_or_else(
                |error| neuromita::ai_runtime::system_hardware::SystemHardware {
                    adapters: Vec::new(),
                    errors: vec![error.to_string()],
                },
            );
    Update::Hardware(neuromita::presentation::hardware::HardwarePresenter::present(inventory))
}

#[cfg(test)]
mod speech_dispatch_tests {
    use super::*;

    #[test]
    fn manual_speech_does_not_trigger_reply() {
        let mut settings = AsrPreferences::default();
        assert_eq!(dispatch_policy(&settings), None);
        settings.instant_send = true;
        settings.delayed_send = false;
        assert_eq!(dispatch_policy(&settings), Some(Duration::ZERO));
        settings.delayed_send = true;
        settings.send_delay_ms = 2400;
        assert_eq!(
            dispatch_policy(&settings),
            Some(Duration::from_millis(2400))
        );
    }

    #[tokio::test]
    async fn speech_sink_only_delivers_transcripts() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let captured = ChatOrigin {
            character: CharacterId::Crazy,
            selection_revision: 4,
        };
        let sink = speech_conversation(&tx, captured);
        let stream = neuromita::audio::ticket::SpeechStream::new(CancellationToken::new());
        let ticket = stream.ticket();
        let transcript = Transcript {
            text: "spoken words".into(),
            language: None,
        };
        sink.reply(&ticket, &transcript).await.unwrap();
        sink.reply(&ticket, &transcript).await.unwrap();
        let (origin, transcript) = rx.recv().await.unwrap();
        assert_eq!(transcript.text, "spoken words");
        assert_eq!(origin, captured);
        assert!(rx.try_recv().is_err(), "duplicate ticket must not requeue");

        let stale = stream.ticket();
        stream.invalidate();
        sink.reply(&stale, &transcript).await.unwrap();
        assert!(rx.try_recv().is_err(), "reset ticket must not enqueue");

        let fresh = stream.ticket();
        sink.reply(&fresh, &transcript).await.unwrap();
        let (fresh_origin, fresh_transcript) = rx.recv().await.unwrap();
        assert_eq!(fresh_origin, captured);
        assert_eq!(fresh_transcript.text, "spoken words");
        assert!(!chat_fence::accepts(
            origin.character.index() as i32,
            origin.selection_revision,
            CharacterId::Crazy.index() as i32,
            6,
        ));
    }
}
