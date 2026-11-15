use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, thread, time::Duration};

use neuromita::{
    ai_runtime::{
        native_download_operation::NativeDownloadSelection,
        native_model::NativeBackend,
        native_revisions::NativeRevisionOverview,
        native_trust::NativeAuthorizedRelease,
        native_workflow::NativeWorkflowSnapshot,
        runtime_status::RuntimeSnapshot,
        speech_catalog::{SpeechInventory, SpeechModelCatalog},
    },
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    audio::{
        controller::SpeechSessionSnapshot, input_catalog::AudioInputDevice,
        monitor::MonitorSnapshot,
    },
    character::CharacterId,
    generation::PresetRecord,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    onboarding::GuideChange,
    presentation::{
        ChatSelection, ConversationChange, ConversationChangePreview, ConversationSnapshot,
        GuiPresenter, NativeHubApi, PresetSlot, native_download::NativeDownloadPresenter,
    },
    remote_voice::{RemoteVoiceCatalogAction, RemoteVoiceConfig, RemoteVoicePreset},
    request::{CancelResult, RequestId},
    settings::{AsrPreferences, PresetId, PresetSelection, TtsPreferences, Volume},
    telegram_admin::TelegramAdmin,
    telegram_voice::DynamicTelegramVoice,
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
    TelegramConnect {
        api_id: String,
        api_hash: String,
        phone: String,
    },
    TelegramCode(String),
    TelegramPassword(String),
    TelegramDisconnect,
    RemoteVoice(RemoteVoiceAction),
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
    RetryGuideChange {
        request_id: u64,
    },
    SetToolOption(ToolOption, bool),
    SetToolDepth(u8),
    SetMemoryDedupEnabled(bool),
    SetMemoryDedupThreshold(u16),
    Select(ChatOrigin, CharacterId),
    Send(ChatOrigin, String),
    DraftChanged(ChatOrigin, String),
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

pub enum RemoteVoiceAction {
    Add,
    Delete,
    SelectWithSave(String, RemoteVoicePreset),
    Save(RemoteVoicePreset),
    Preview(String, Option<CharacterId>, RemoteVoicePreset),
    Cancel,
}

pub enum Update {
    RuntimeStatus(RuntimeSnapshot),
    RemoteVoices(RemoteVoiceConfig),
    RemoteVoiceState(String, bool),
    SpeechSessionStatus(SpeechSessionSnapshot),
    MicrophoneTestStatus(MonitorSnapshot),
    SpeechDraft(neuromita::presentation::SpeechDraftSnapshot),
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
    Selection(ChatSelection),
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
    GuideRejected {
        request_id: u64,
        error: String,
    },
    TelegramAuth {
        phase: i32,
        status: String,
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

async fn enable_speech_session(
    app: &ApplicationLifecycle,
    directory: &std::path::Path,
    origin: ChatOrigin,
) -> Result<(), String> {
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
    app.presenter()
        .selection
        .start_capture_and_press(
            ChatSelection {
                character: origin.character,
                revision: origin.selection_revision,
            },
            ffmpeg,
            ffplay,
            matches!(
                app.presenter()
                    .settings
                    .asr
                    .preferences
                    .get()
                    .input_mode
                    .as_str(),
                "radio" | "push_to_talk"
            ),
        )
        .await
}

async fn start(
    directory: &std::path::Path,
    model: Option<&str>,
    telegram_voice: Arc<DynamicTelegramVoice>,
    telegram_admin: Arc<TelegramAdmin>,
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
    config.voice = telegram_voice;
    config.telegram_admin = Some(telegram_admin);
    config.remote_voice_store = Some(directory.join("remote_voice.rust.json"));
    ApplicationLifecycle::start(config)
        .await
        .map_err(|e| e.to_string())
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
    let telegram_voice = Arc::new(DynamicTelegramVoice::default());
    let telegram_admin = Arc::new(TelegramAdmin::new(
        directory.clone(),
        telegram_voice.clone(),
    ));
    let app = match start(
        &directory,
        initial_model.as_deref(),
        telegram_voice.clone(),
        telegram_admin.clone(),
    )
    .await
    {
        Ok(app) => app,
        Err(error) => {
            publish(Update::Error(error));
            publish(Update::Stopped);
            serve_offline_guide(&directory, &mut receive, &stop, &publish).await;
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

async fn serve_offline_guide(
    directory: &std::path::Path,
    receive: &mut mpsc::Receiver<Command>,
    stop: &CancellationToken,
    publish: &impl Fn(Update),
) {
    loop {
        let command = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            command = receive.recv() => command,
        };
        let Some(command) = command else { return };
        if let Command::ChangeGuide { request_id, change } = command {
            let directory = directory.to_owned();
            let operation = change.clone();
            let result = tokio::task::spawn_blocking(move || operation.persist_offline(&directory))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            match result {
                Ok(()) => publish(Update::GuidePersisted { request_id, change }),
                Err(error) => publish(Update::GuideRejected { request_id, error }),
            }
        }
    }
}

pub fn native_download_configured() -> bool {
    neuromita::ai_runtime::native_service::native_download_configured()
}

async fn publish_native_release_catalog(native: &NativeHubApi, publish: &impl Fn(Update)) {
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

async fn publish_native(native: &NativeHubApi, publish: &impl Fn(Update)) {
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

fn remote_message(phase: &str) -> &'static str {
    match phase {
        "previewing" => "Синтез и воспроизведение…",
        "completed" => "Голос воспроизведён",
        "cancelled" => "Проверка отменена",
        "saved" => "Облачный профиль сохранён",
        "failed" => "Проверка не удалась",
        _ => "",
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
    let remote_admin = administration
        .remote_voice
        .as_ref()
        .ok_or("Remote Voice administration was not initialized")?;
    let mut remote_updates = remote_admin.observe();
    let mut remote_revision = remote_admin.snapshot().revision;
    publish(Update::RemoteVoices(remote_admin.config()));
    let telegram_admin = administration
        .telegram
        .as_ref()
        .ok_or("Telegram Presenter is not configured")?;
    let mut telegram_updates = telegram_admin.observe();
    let mut telegram_watch_closed = false;
    let mut releases = administration.releases.observe();
    let presenter = &administration.conversation;
    let mut connections = administration.connections.observe();
    let mut memories = MemoryWatch::start(administration, CharacterId::Crazy)?;
    let (recall_updates, mut recall_results) = mpsc::unbounded_channel();
    let mut pending: Option<(ChatOrigin, ConversationChangePreview)> = None;
    let telegram_status = telegram_admin.snapshot();
    publish(Update::TelegramAuth {
        phase: i32::from(telegram_status.phase),
        status: format!("Telegram: {}", telegram_status.status),
    });
    let current = administration.selection.snapshot();
    let mut character = current.character;
    let mut selection_revision = current.revision;
    let mut selected = administration.selection.observe();
    publish(Update::Selection(current));
    let mut debug_character = CharacterId::Crazy;
    let mut requests = presenter.observe_requests();
    let mut settings = presenter.observe_settings().map_err(|e| e.to_string())?;
    let mut presets = presenter.observe_presets();
    let mut selection = presenter.observe_selection().map_err(|e| e.to_string())?;
    let mut runtime_status = administration.ai_runtime.observe();
    let mut speech_status = administration.speech.observe();
    let mut microphone_test = administration.audio_input.observe_test();
    let install_owner = administration
        .ai_install
        .as_ref()
        .ok_or("desktop has no AI Hub install Presenter")?;
    let mut install_updates = install_owner.observe();
    publish_ai_install(&install_owner.status(), publish);
    let mut draft_updates = administration.speech_chat.observe();
    publish(Update::SpeechDraft(
        draft_updates.borrow_and_update().clone(),
    ));
    let mut liveness = tokio::time::interval(Duration::from_secs(5));
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let native_owner = administration
        .native_hub
        .as_ref()
        .ok_or("desktop has no NativeHub Presenter")?;
    let mut native_download_updates = native_owner.observe_download();
    publish_native_download(&native_download_updates.borrow().clone(), publish);
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
                        prompt_sets: administration.prompt_folder.inventory().sets,
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
            _ = stop.cancelled() => {
                return Ok(());
            },
            changed = telegram_updates.changed(), if !telegram_watch_closed => {
                if changed.is_ok() {
                    let status = telegram_updates.borrow_and_update().clone();
                    publish(Update::TelegramAuth {
                        phase: i32::from(status.phase),
                        status: format!("Telegram: {}", status.status),
                    });
                } else {
                    telegram_watch_closed = true;
                }
            },
            changed = selected.changed() => {
                changed.map_err(|_| "Chat selection observer closed")?;
                let value = *selected.borrow_and_update();
                character = value.character;
                selection_revision = value.revision;
                pending = None;
                publish(Update::Selection(value));
                publish(Update::Confirmation(ChatOrigin {
                    character, selection_revision,
                }, None));
            },
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
                    match administration.guide_persistence.change(change.clone()).await {
                        Ok(()) => publish(Update::GuidePersisted { request_id, change }),
                        Err(neuromita::presentation::GuideSaveError::Failed(error)) =>
                            publish(Update::GuideFailed { request_id, error }),
                        Err(neuromita::presentation::GuideSaveError::Rejected(error)) =>
                            publish(Update::GuideRejected { request_id, error }),
                    }
                },
                Some(Command::RetryGuideChange { request_id }) => {
                    match administration.guide_persistence.retry().await {
                        Ok(change) => publish(Update::GuidePersisted { request_id, change }),
                        Err(neuromita::presentation::GuideSaveError::Failed(error)) =>
                            publish(Update::GuideFailed { request_id, error }),
                        Err(neuromita::presentation::GuideSaveError::Rejected(error)) =>
                            publish(Update::GuideRejected { request_id, error }),
                    }
                },
                Some(command @ (Command::DeletePreset(_) | Command::MovePreset(..) | Command::ActivePreset(_) | Command::AssignPreset(..) | Command::AssignAllPresets(..) | Command::SelectPromptSet(..) | Command::VoiceInLauncher(_) | Command::VoiceInGame(_) | Command::VoiceName(_) | Command::SectionVisibility(..) | Command::HidePrivate(_) | Command::StructuredDisplay(_) | Command::SetToolOption(..) | Command::SetToolDepth(_) | Command::SetMemoryDedupEnabled(_) | Command::SetMemoryDedupThreshold(_) | Command::SaveAsr(..) | Command::SaveCharacterVoice(..) | Command::SelectAsrRecognizer(_))) => {
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
                Some(Command::SaveTts(value)) => {
                    match super::settings::execute(administration, Command::SaveTts(value)) {
                        Ok(()) => {
                            if let Err(error) = telegram_admin.refresh_voice().await {
                                publish(Update::AdminError(error));
                            }
                            let status = telegram_admin.snapshot();
                            publish(Update::TelegramAuth { phase: i32::from(status.phase), status: format!("Telegram: {}", status.status) });
                            publish(Update::AdminSaved);
                        }
                        Err(error) => publish(Update::AdminError(error)),
                    }
                },
                Some(Command::TelegramConnect { api_id, api_hash, phone }) => {
                    if let Err(error) = telegram_admin.connect(&api_id, &api_hash, &phone).await {
                        publish(Update::AdminError(error));
                    }
                    let status = telegram_admin.snapshot();
                    publish(Update::TelegramAuth { phase: i32::from(status.phase), status: format!("Telegram: {}", status.status) });
                },
                Some(Command::TelegramCode(code)) => {
                    if let Err(error) = telegram_admin.submit_code(&code).await {
                        publish(Update::AdminError(error));
                    }
                    let status = telegram_admin.snapshot();
                    publish(Update::TelegramAuth { phase: i32::from(status.phase), status: format!("Telegram: {}", status.status) });
                },
                Some(Command::TelegramPassword(password)) => {
                    if let Err(error) = telegram_admin.submit_password(&password).await {
                        publish(Update::AdminError(error));
                    }
                    let status = telegram_admin.snapshot();
                    publish(Update::TelegramAuth { phase: i32::from(status.phase), status: format!("Telegram: {}", status.status) });
                },
                Some(Command::TelegramDisconnect) => {
                    let status = telegram_admin.disconnect().await;
                    publish(Update::TelegramAuth { phase: i32::from(status.phase), status: format!("Telegram: {}", status.status) });
                },
                Some(Command::RemoteVoice(action)) => {
                    let result = match action {
                        RemoteVoiceAction::Preview(text, character, preset) => {
                            remote_admin.preview(text, character, preset, Some(remote_revision))
                                .map_err(|error| error.to_string())
                        }
                        RemoteVoiceAction::Cancel => Ok(remote_admin.cancel()),
                        action => {
                            let change = match action {
                                RemoteVoiceAction::Add => RemoteVoiceCatalogAction::Add,
                                RemoteVoiceAction::Delete => RemoteVoiceCatalogAction::Delete,
                                RemoteVoiceAction::SelectWithSave(id, preset) =>
                                    RemoteVoiceCatalogAction::SelectWithSave(id, preset),
                                RemoteVoiceAction::Save(preset) => RemoteVoiceCatalogAction::Save(preset),
                                RemoteVoiceAction::Preview(..) | RemoteVoiceAction::Cancel => unreachable!(),
                            };
                            remote_admin.change(change, Some(remote_revision)).map_err(|e| e.to_string())
                        }
                    };
                    match result {
                        Ok(state) => {
                            remote_revision = state.revision;
                            publish(Update::RemoteVoices(remote_admin.config()));
                            publish(Update::RemoteVoiceState(remote_message(&state.phase).into(), state.busy));
                        }
                        Err(error) => {
                            publish(Update::RemoteVoiceState(error, remote_admin.snapshot().busy));
                        }
                    }
                },
                Some(Command::NewPreset) => match administration.presets.create_unconfigured() {
                    Ok((id, record)) => publish(Update::PresetCreated(id, record)),
                    Err(error) => publish(Update::AdminError(error.to_string())),
                },
                Some(Command::SpeechSessionEnabled(enabled)) => {
                    let result = if enabled {
                        enable_speech_session(app, directory, ChatOrigin {
                            character, selection_revision,
                        }).await
                    } else {
                        administration.selection.stop_capture().await;
                        Ok(())
                    };
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
                        enable_speech_session(app, directory, origin).await
                    } else {
                        administration.selection.stop_capture().await;
                        Ok(())
                    };
                    if let Err(error) = result { publish(Update::ChatError(origin, error)); }
                    publish(Update::SpeechSessionStatus(administration.speech.snapshot()));
                },
                Some(Command::SpeechPushToTalk(pressed)) => {
                    if let Err(error) = administration.selection.press(pressed).await && pressed {
                        publish(Update::AdminError(error));
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
                    let result = match native_backend(&backend) {
                        Some(backend) => native_owner.start_download(
                            NativeDownloadSelection { model_id, manifest_sha256, backend }
                        ).await,
                        None => Err("Неизвестный native backend".into()),
                    };
                    if let Err(error) = result {
                        publish(Update::NativeError(error));
                    }
                    publish_native_download(&native_owner.download_snapshot(), publish);
                    refresh_views = false;
                },
                Some(Command::CancelNativePackageInstall) => {
                    native_owner.cancel_download().await;
                    publish_native_download(&native_owner.download_snapshot(), publish);
                    refresh_views = false;
                },
                Some(Command::RefreshPromptCatalog) => {},
                Some(Command::DebugSelect(id)) => { debug_character = id; },
                Some(Command::OpenPromptFolder(character)) => {
                    if let Err(error) = administration.prompt_folder.open(character) {
                        publish(Update::AdminError(error));
                    }
                },
                Some(Command::RefreshHardware) => publish(hardware_snapshot().await),
                Some(Command::PreviewAiHubInstall { category, id, token }) => {
                    install_owner.preview(category, id, token).await;
                    refresh_views = false;
                },
                Some(Command::ConfirmAiHubInstall { category, id, token }) => {
                    if let Err(error) = install_owner.confirm(category.clone(), id.clone(), token).await {
                        publish(Update::AiHubInstallPreview {
                            category, id, token, summary: String::new(), error: error.into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new(),
                        });
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
                    administration.graph.reset().await;
                    memories = MemoryWatch::start(administration, id)?;
                },
                Some(Command::GraphPreview(request_id, selected, memory_id, mode)) => {
                    let result = administration.graph.preview(request_id, selected, memory_id, mode).await;
                    match result {
                        Ok(review) => publish(Update::GraphPreviewResult(request_id, Ok(GraphPreviewReport {
                            count: review.relation_count,
                            relations: review.relations,
                        }))),
                        Err(error) => publish(Update::GraphPreviewResult(request_id, Err(error))),
                    }
                },
                Some(Command::GraphApply(request_id, selected)) => {
                    publish(Update::GraphApplyResult(request_id,
                        administration.graph.apply(request_id, selected).await));
                },
                Some(Command::GraphAbort(request_id)) => {
                    administration.graph.abort(request_id).await;
                },
                Some(Command::MemoryEdit(edit_id, expected, commands)) => {
                    match administration.characters.edit_memories(expected.character(), expected, commands).await {
                        Ok(_) => publish(Update::MemorySaved(edit_id)), Err(error) => publish(Update::MemoryEditError(edit_id, error.to_string())),
                    }
                },
                Some(Command::MemoryRecall(request_id, character, memory_id, query)) => {
                    let sender = recall_updates.clone();
                    let cancelled = stop.clone();
                    let characters = administration.characters.clone();
                    tokio::spawn(async move {
                        let result = tokio::select! {
                            _ = cancelled.cancelled() => return,
                            result = characters.recall_forgotten(character, memory_id, &query) => result,
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
                        let text = match administration.chat.latest_diagnostics(origin.character) {
                            Ok(Some((_, text))) => text,
                            Ok(None) => "No retained request trace".into(),
                            Err(error) => format!("Cannot export request diagnostics: {error}"),
                        };
                        publish(Update::Diagnostics(origin, text));
                    }
                },
                Some(Command::Select(origin, new_character)) => {
                    let expected = ChatSelection {
                        character: origin.character,
                        revision: origin.selection_revision,
                    };
                    if let Err(error) = administration.selection.select(expected, new_character).await {
                        publish(Update::AdminError(error));
                    }
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
                Some(Command::DraftChanged(origin, text)) => {
                    if let Err(error) = administration.speech_chat.set_draft(ChatSelection {
                        character: origin.character,
                        revision: origin.selection_revision,
                    }, text).await {
                        publish(Update::ChatError(origin, error));
                    }
                    refresh_views = false;
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
                        match administration.speech_chat.send(ChatSelection {
                            character: origin.character,
                            revision: origin.selection_revision,
                        }, text.clone()).await {
                            Ok(_) => publish(Update::Sent(origin, text)),
                            Err(error) => publish(Update::ChatError(origin, error)),
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
                    let result = administration.conversation.current_fence().and_then(|fence| {
                        administration.conversation.attach_file(
                            origin.character,
                            text.clone(),
                            std::path::Path::new(&source),
                            fence,
                        )
                    }).map(|_| ());
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
            changed = install_updates.changed() => {
                changed.map_err(|_| "AI Hub install observer closed")?;
                publish_ai_install(&install_updates.borrow_and_update().clone(), publish);
                refresh_views = false;
            },
            changed = remote_updates.changed() => {
                changed.map_err(|_| "Remote Voice observer closed")?;
                let state = remote_updates.borrow_and_update().clone();
                remote_revision = state.revision;
                publish(Update::RemoteVoices(remote_admin.config()));
                publish(Update::RemoteVoiceState(remote_message(&state.phase).into(), state.busy));
                refresh_views = false;
            },
            changed = draft_updates.changed() => {
                changed.map_err(|_| "Speech draft Presenter observation closed")?;
                publish(Update::SpeechDraft(draft_updates.borrow_and_update().clone()));
                refresh_views = false;
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
            changed = native_download_updates.changed() => {
                changed.map_err(|_| "Native download observer closed")?;
                let status = native_download_updates.borrow_and_update().clone();
                publish_native_download(&status, publish);
                if status.phase == "installed" {
                    publish_native(native_owner, publish).await;
                    publish_native_release_catalog(native_owner, publish).await;
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

fn publish_ai_install(
    status: &neuromita::ai_runtime::install_coordinator::InstallStatus,
    publish: &impl Fn(Update),
) {
    let view = neuromita::presentation::AiHubInstallPresenter::project(status);
    if !view.operation_id.is_empty() {
        publish(Update::AiHubInstallOperation {
            operation_id: view.operation_id.clone(),
            category: view.category.clone(),
            id: view.id.clone(),
            stage: view.stage.clone(),
            summary: view.operation_summary,
        });
    }
    publish(Update::AiHubInstallPreview {
        category: view.category,
        id: view.id,
        token: view.token,
        summary: view.summary,
        error: view.error,
        stage: view.stage,
        can_install: view.can_install,
        operation_id: view.operation_id,
    });
}

fn publish_native_download(
    state: &neuromita::ai_runtime::native_service::NativeDownloadStatus,
    publish: &impl Fn(Update),
) {
    let view = NativeDownloadPresenter::status(state);
    publish(Update::NativeDownloadStatus {
        summary: view.summary,
        percent: view.percent,
        active: view.active,
    });
}

async fn character_debug_snapshot(api: &GuiPresenter, character: CharacterId) -> Update {
    let result = neuromita::presentation::project_character_debug(
        &api.characters,
        api.settings.as_ref(),
        api.prompts.as_deref(),
        character,
    )
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
