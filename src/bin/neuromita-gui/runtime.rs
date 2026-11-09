use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, thread, time::Duration};

use neuromita::{
    ai_runtime::{
        install_admission::{
            ApprovedInstall, InstallAction, InstallActionReview, InstallApprovalGate,
            install_reviewed_overlay, install_reviewed_wheel_stage, preview_install_action,
        },
        install_review::{InstallPlanReview, InstallPreviewError, InstallReviewFence},
        native_model::NativeBackend,
        native_revisions::{NativeRevisionOverview, NativeRevisionStore},
        native_trust::{NativePolicyPin, NativeReleaseTrust},
        native_workflow::{NativeModelWorkflow, NativeWorkflowSnapshot},
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
        PresetSlot,
    },
    request::{CancelResult, RequestId},
    settings::{AsrPreferences, PresetId, PresetSelection, TtsPreferences, Volume},
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

pub enum Command {
    RefreshReleases,
    MemorySelect(CharacterId),
    MemoryEdit(
        uuid::Uuid,
        neuromita::character::CharacterMemorySnapshot,
        Vec<neuromita::memory::MemoryCommand>,
    ),
    Preview(ConversationChange),
    Confirm,
    Abort,
    Diagnostics,
    NewPreset,
    UpdatePreset(PresetId, neuromita::generation::PresetConnectionEdit),
    DeletePreset(PresetId),
    ActivePreset(PresetId),
    AssignPreset(CharacterId, PresetSlot, PresetSelection),
    AssignAllPresets(CharacterId, PresetSelection),
    SelectPromptSet(CharacterId, String),
    DebugSelect(CharacterId),
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
    },
    StopNativeModel,
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
    SetToolOption(ToolOption, bool),
    SetToolDepth(u8),
    Select(CharacterId),
    Send(String),
    Cancel(RequestId),
    Refresh,
    Volume(u8),
    SpeechSessionEnabled(bool),
    SpeechPushToTalk(bool),
    MicrophoneTest(bool, String, String),
}

pub enum Update {
    RuntimeStatus(RuntimeSnapshot),
    SpeechSessionStatus(SpeechSessionSnapshot),
    MicrophoneTestStatus(MonitorSnapshot),
    RecognizedText {
        text: String,
        auto_send_after: Option<Duration>,
        merge_with_draft: bool,
    },
    Hardware(neuromita::presentation::hardware::HardwareView),
    CharacterCatalog(Vec<neuromita::character::CharacterDefinition>),
    SpeechInventory(Box<SpeechInventory>, Vec<AudioInputDevice>),
    NativeModels(Box<NativeWorkflowSnapshot>),
    NativeError(String),
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
    Confirmation(Option<(ConversationChange, usize)>),
    Diagnostics(String),
    Connections(std::sync::Arc<neuromita::server::ServerSnapshot>),
    Administration(Box<super::settings::AdministrationSnapshot>),
    CharacterDebug {
        character: CharacterId,
        fields: Vec<neuromita::presentation::CharacterDebugField>,
        error: Option<String>,
    },
    PresetCreated(PresetId, PresetRecord),
    AdminSaved,
    PresetSaved(PresetId),
    AdminError(String),
    Stopped,
    Connecting,
    Connected(String),
    Snapshot(Box<ConversationSnapshot>),
    Sent(String),
    Error(String),
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

struct DesktopTranscriptSink(mpsc::UnboundedSender<Transcript>);

impl SpeechConversation for DesktopTranscriptSink {
    fn reply<'a>(
        &'a self,
        transcript: &'a Transcript,
    ) -> SpeechFuture<'a, Option<neuromita::voice::AudioArtifact>> {
        Box::pin(async move {
            self.0
                .send(transcript.clone())
                .map_err(|_| SpeechSessionError::DispatchClosed)?;
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
    );
    let root = std::env::var_os("NEUROMITA_PROMPTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| directory.join("Prompts"));
    let config = config.with_prompt_catalog(neuromita::prompts::PromptCatalog::new(root));
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
    publish(Update::Stopped);
}

fn open_native_workflow(
    directory: &std::path::Path,
) -> Result<Option<NativeModelWorkflow>, String> {
    let native = directory.join("AIData/NativeModels/revisions");
    let runtime = directory.join("AIData/NativeRuntime");
    let native_exists = native.try_exists().map_err(|error| error.to_string())?;
    let runtime_exists = runtime.try_exists().map_err(|error| error.to_string())?;
    if !native_exists && !runtime_exists {
        return Ok(None);
    }
    if !native_exists || !runtime_exists {
        return Err("Нативное хранилище установлено не полностью".into());
    }
    let policy_pin = option_env!("NEUROMITA_NATIVE_PUBLISHER_POLICY_SHA256")
        .map(NativePolicyPin::from_sha256_hex)
        .transpose()
        .map_err(|error| error.to_string())?;
    let workflow = NativeModelWorkflow::with_trust(
        NativeRevisionStore::new(native, runtime),
        NativeReleaseTrust::new(
            directory.join("Config/native-publisher-keys.json"),
            directory.join("AIData/NativeApprovals"),
            policy_pin,
        ),
    );
    workflow.snapshot().map_err(|error| error.to_string())?;
    Ok(Some(workflow))
}

fn publish_native(workflow: &Option<NativeModelWorkflow>, publish: &impl Fn(Update)) {
    let snapshot = match workflow.as_ref() {
        Some(workflow) => workflow.snapshot().map_err(|error| error.to_string()),
        None => Ok(NativeWorkflowSnapshot {
            trust: Vec::new(),
            inventory: NativeRevisionOverview {
                revisions: Vec::new(),
                selections: Vec::new(),
            },
            active_revision: None,
            worker: None,
        }),
    };
    match snapshot {
        Ok(snapshot) => publish(Update::NativeModels(Box::new(snapshot))),
        Err(_) => publish(Update::NativeError(
            "Не удалось прочитать каталог нативных ревизий".into(),
        )),
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
    let mut pending: Option<ConversationChangePreview> = None;
    let mut character = CharacterId::Crazy;
    let mut debug_character = CharacterId::Crazy;
    let mut requests = presenter.observe_requests();
    let mut settings = presenter.observe_settings().map_err(|e| e.to_string())?;
    let mut presets = presenter.observe_presets();
    let mut selection = presenter.observe_selection().map_err(|e| e.to_string())?;
    let mut runtime_status = administration.ai_runtime.observe();
    let mut speech_status = administration.speech.observe();
    let mut microphone_test = administration.audio_input.observe_test();
    let (plan_results, mut plan_updates) = mpsc::unbounded_channel::<(
        u64,
        u64,
        String,
        String,
        i32,
        Result<InstallActionReview, InstallPreviewError>,
    )>();
    let mut plan_fence = InstallReviewFence::default();
    let mut active_install_preview: Option<tokio::task::AbortHandle> = None;
    let mut approvals = InstallApprovalGate::default();
    let (install_results, mut install_updates) = mpsc::unbounded_channel::<(
        uuid::Uuid,
        String,
        String,
        i32,
        InstallAction,
        Result<std::path::PathBuf, InstallPreviewError>,
    )>();
    let mut active_install: Option<uuid::Uuid> = None;
    let (transcripts, mut transcript_updates) = mpsc::unbounded_channel();
    let transcript_sink: Arc<dyn SpeechConversation> = Arc::new(DesktopTranscriptSink(transcripts));
    let mut liveness = tokio::time::interval(Duration::from_secs(5));
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let initial_native = open_native_workflow(directory);
    let native_error = initial_native.is_err();
    let mut native_workflow = initial_native.unwrap_or(None);
    publish_native(&native_workflow, publish);
    if native_error {
        publish(Update::NativeError(
            "Ошибка при обнаружении нативных моделей".into(),
        ));
    }
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
                Ok(snapshot) => publish(Update::Snapshot(Box::new(snapshot))),
                Err(error) => publish(Update::Error(error.to_string())),
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
                if let Some(workflow) = native_workflow.as_mut() {
                    if workflow.snapshot().ok().and_then(|snapshot| snapshot.worker).is_some() {
                        let _ = workflow.deactivate().await;
                    }
                }
                return Ok(());
            },
            command = receive.recv() => match command {
                Some(command @ Command::UpdatePreset(id, _)) => {
                    match super::settings::execute(administration, command) {
                        Ok(()) => publish(Update::PresetSaved(id)),
                        Err(error) => publish(Update::AdminError(error)),
                    }
                },
                Some(command @ (Command::DeletePreset(_) | Command::ActivePreset(_) | Command::AssignPreset(..) | Command::AssignAllPresets(..) | Command::SelectPromptSet(..) | Command::VoiceInLauncher(_) | Command::VoiceInGame(_) | Command::VoiceName(_) | Command::SectionVisibility(..) | Command::HidePrivate(_) | Command::StructuredDisplay(_) | Command::SetToolOption(..) | Command::SetToolDepth(_) | Command::SaveTts(_) | Command::SaveAsr(..) | Command::SaveCharacterVoice(..) | Command::SelectAsrRecognizer(_))) => {
                    match super::settings::execute(administration, command) {
                        Ok(()) => publish(Update::AdminSaved),
                        Err(error) => publish(Update::AdminError(error)),
                    }
                },
                Some(Command::NewPreset) => match administration.presets.create_unconfigured() {
                    Ok((id, record)) => publish(Update::PresetCreated(id, record)),
                    Err(error) => publish(Update::AdminError(error.to_string())),
                },
                Some(Command::SpeechSessionEnabled(enabled)) => {
                    let result = if enabled {
                        enable_speech_session(app, directory, Arc::clone(&transcript_sink)).await
                    } else {
                        app.stop_speech_capture().await;
                        Ok(())
                    };
                    if let Err(error) = result { publish(Update::AdminError(error)); }
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
                    let mut failed = false;
                    if native_workflow.is_none() {
                        match open_native_workflow(directory) {
                            Ok(workflow) => native_workflow = workflow,
                            Err(_) => failed = true,
                        }
                    }
                    publish_native(&native_workflow, publish);
                    if failed {
                        publish(Update::NativeError("Ошибка при обнаружении нативных моделей".into()));
                    }
                    refresh_views = false;
                },
                Some(Command::SelectNativeRevision { model_id, manifest_sha256, backend }) => {
                    let result = match (native_workflow.as_ref(), native_backend(&backend)) {
                        (Some(workflow), Some(backend)) => workflow.select_trusted(&model_id, &manifest_sha256, backend).map_err(|_| "Выбор отклонён: необходимы подтверждённая подпись издателя и целостный совместимый пакет"),
                        _ => Err("Нативная ревизия или backend недоступны"),
                    };
                    publish_native(&native_workflow, publish);
                    if let Err(error) = result { publish(Update::NativeError(error.into())); }
                    refresh_views = false;
                },
                Some(Command::ActivateNativeModel { model_id }) => {
                    let result = match native_workflow.as_mut() {
                        Some(workflow) => workflow.activate_selected(&model_id, Duration::from_secs(30)).await.map(|_| ()),
                        None => Err(neuromita::ai_runtime::native_workflow::NativeWorkflowError::NotSelected),
                    };
                    publish_native(&native_workflow, publish);
                    if result.is_err() { publish(Update::NativeError("Запуск отклонён: подпись издателя, выбранная ревизия, файлы или ответ worker не подтверждены".into())); }
                    refresh_views = false;
                },
                Some(Command::StopNativeModel) => {
                    let result = match native_workflow.as_mut() {
                        Some(workflow) => workflow.deactivate().await,
                        None => Err(neuromita::ai_runtime::native_workflow::NativeWorkflowError::NotActive),
                    };
                    publish_native(&native_workflow, publish);
                    if result.is_err() { publish(Update::NativeError("Завершение нативного worker не подтверждено".into())); }
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
                    if let Some(handle) = active_install_preview.take() { handle.abort(); }
                    approvals.invalidate();
                    let (request, generation) = plan_fence.begin();
                    let service = match category.as_str() {
                        "tts" => Some(neuromita::ai_runtime::AiService::Tts),
                        "asr" => Some(neuromita::ai_runtime::AiService::Asr),
                        _ => None,
                    };
                    let available = if let Some(service) = service {
                        let snapshot = catalog.snapshot().await;
                        let matching = match service {
                            neuromita::ai_runtime::AiService::Tts => &snapshot.tts,
                            neuromita::ai_runtime::AiService::Asr => &snapshot.asr,
                            _ => unreachable!(),
                        };
                        matching.iter().filter(|model| model.id == id).count() == 1
                    } else {
                        false
                    };
                    if let Some(service) = service.filter(|_| available) {
                        let send = plan_results.clone();
                        let data_directory = directory.to_path_buf();
                        let model_id = id.clone();
                        let task = tokio::spawn(async move {
                            let result = preview_install_action(&data_directory, service, &model_id).await;
                            let _ = send.send((request, generation, category, id, token, result));
                        });
                        active_install_preview = Some(task.abort_handle());
                    } else {
                        publish(Update::AiHubInstallPreview {
                            category, id, token, summary: String::new(),
                            error: "Модель отсутствует или неоднозначна в актуальном каталоге".into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new(),
                        });
                    }
                },
                Some(Command::ConfirmAiHubInstall { category, id, token }) => {
                    if active_install.is_some() {
                        publish(Update::AiHubInstallPreview { category, id, token,
                            summary: String::new(), error: "Установка уже выполняется".into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new() });
                        continue;
                    }
                    let service = match category.as_str() {
                        "tts" => Some(neuromita::ai_runtime::AiService::Tts),
                        "asr" => Some(neuromita::ai_runtime::AiService::Asr),
                        _ => None,
                    };
                    let Some(ApprovedInstall { review, action }) =
                        service.and_then(|service| approvals.consume_action(token, service, &id)) else {
                        publish(Update::AiHubInstallPreview { category, id, token,
                            summary: String::new(), error: "План не подтверждён или устарел".into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new() });
                        continue;
                    };
                    if matches!(action, InstallAction::Overlay) && !review.eligible_overlay_install() {
                        publish(Update::AiHubInstallPreview { category, id, token,
                            summary: String::new(), error: "Выбранная модель или план изменились".into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new() });
                        continue;
                    }
                    let service = review.identity.service;
                    let inventory = catalog.snapshot().await;
                    let matches = match service {
                        neuromita::ai_runtime::AiService::Tts => &inventory.tts,
                        neuromita::ai_runtime::AiService::Asr => &inventory.asr,
                        _ => unreachable!(),
                    }.iter().filter(|item| item.id == id).count();
                    if matches != 1 {
                        publish(Update::AiHubInstallPreview { category, id, token,
                            summary: String::new(), error: "Модель исчезла из актуального каталога".into(),
                            stage: "failed".into(), can_install: false, operation_id: String::new() });
                        continue;
                    }
                    let operation = uuid::Uuid::new_v4();
                    active_install = Some(operation);
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
                    publish(Update::AiHubInstallPreview { category: category.clone(), id: id.clone(), token,
                        summary: preview_summary.into(), error: String::new(),
                        stage: "installing".into(), can_install: false,
                        operation_id: operation.to_string() });
                    let sender = install_results.clone();
                    let path = directory.to_path_buf();
                    tokio::spawn(async move {
                        let result = match &action {
                            InstallAction::Overlay => install_reviewed_overlay(&path, &review).await,
                            InstallAction::Wheel { stage_id, backend } =>
                                install_reviewed_wheel_stage(&path, &review, stage_id, backend).await,
                        };
                        let _ = sender.send((operation, category, id, token, action, result));
                    });
                },
                Some(Command::RefreshSpeechInventory) => {
                    approvals.invalidate();
                    plan_fence.catalog_replaced();
                    if let Some(handle) = active_install_preview.take() { handle.abort(); }

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
                Some(Command::MemorySelect(id)) => { memories = MemoryWatch::start(administration, id)?; },
                Some(Command::MemoryEdit(edit_id, expected, commands)) => {
                    match administration.characters.edit_memories(expected.character(), expected, commands).await {
                        Ok(_) => publish(Update::MemorySaved(edit_id)), Err(error) => publish(Update::MemoryEditError(edit_id, error.to_string())),
                    }
                },
                Some(Command::Preview(operation)) => match presenter.preview_change(character, operation).await {
                    Ok(preview) => { publish(Update::Confirmation(Some((preview.operation(), preview.message_count())))); pending = Some(preview); },
                    Err(error) => publish(Update::Error(error.to_string())),
                },
                Some(Command::Confirm) => {
                    if let Some(preview) = pending.take() && let Err(error) = presenter.apply_change(preview) { publish(Update::Error(error.to_string())); }
                    publish(Update::Confirmation(None));
                },
                Some(Command::Abort) => { pending = None; publish(Update::Confirmation(None)); },
                Some(Command::Diagnostics) => {
                    let model = administration.chat.snapshot();
                    let trace = model.requests.iter().rev().find(|r| r.metadata.character_id == character).and_then(|r| administration.chat.trace(r.metadata.id));
                    let text = match trace {
                        Some(trace) => match trace.section_json(neuromita::request::DiagnosticSection::All) {
                            Ok(text) => text,
                            Err(error) => format!("Cannot export request diagnostics: {error}"),
                        },
                        None => "No retained request trace".into(),
                    };
                    publish(Update::Diagnostics(text));
                },
                Some(Command::Select(id)) => { character = id; pending = None; publish(Update::Confirmation(None)); },
                Some(Command::Refresh) => {},
                Some(Command::Send(text)) => match presenter.reply(character, text.clone()) {
                    Ok(_) => publish(Update::Sent(text)),
                    Err(error) => publish(Update::Error(error.to_string())),
                },
                Some(Command::Cancel(id)) => match presenter.cancel(id) {
                    Ok(CancelResult::Cancelled | CancelResult::Requested) => {},
                    Ok(CancelResult::TooLate | CancelResult::AlreadyFinished) =>
                        publish(Update::Error("The result has already been accepted; cancellation is too late".into())),
                    Err(error) => publish(Update::Error(error.to_string())),
                },
                Some(Command::Volume(value)) => match Volume::new(value).and_then(|v| presenter.set_volume(v)) {
                    Ok(()) => {},
                    Err(error) => publish(Update::AdminError(error.to_string())),
                },
            },
            installed = install_updates.recv() => {
                if let Some((operation, category, id, token, action, result)) = installed
                    && active_install == Some(operation)
                {
                    active_install = None;
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
                }
            },
            reviewed = plan_updates.recv() => {
                if let Some((request, generation, category, id, token, result)) = reviewed
                    && plan_fence.accepts(request, generation)
                {
                    active_install_preview = None;
                    let (summary, error, can_install, stage) = match result {
                        Ok(InstallActionReview { review, action }) => {
                            let mut summary = summarize_ai_hub_review(&review);
                            let (permitted, review_stage) = match action {
                                Some(InstallAction::Overlay) if active_install.is_none() =>
                                    (approvals.offer(token, review), "review"),
                                Some(InstallAction::Wheel { stage_id, backend }) if active_install.is_none() => {
                                    summary.push_str(&format!(
                                        "\nДоступен проверенный wheel-stage: {stage_id} · {backend}. Установится только этот пакет, не модель.",
                                    ));
                                    (approvals.offer_wheel(token, review, &stage_id, &backend), "wheel-review")
                                },
                                _ => {
                                    summary.push_str("\nИсполнение недоступно: нет единственного проверенного и локально доступного wheel-stage либо необходимы другие установочные этапы.");
                                    (false, "review")
                                },
                            };
                            (summary, String::new(), permitted, review_stage.to_owned())
                        },
                        Err(InstallPreviewError::InvalidSelection) => (String::new(),
                            "Модель недоступна или исходник не прошёл проверку безопасности.".into(), false, "failed".into()),
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
                }
            },
            transcript = transcript_updates.recv() => {
                if let Some(transcript) = transcript {
                    let text = transcript.text.trim();
                    if !text.is_empty() {
                        let preferences = administration.settings.asr.preferences.get();
                        publish(Update::RecognizedText {
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
            result = requests.changed() => { result.map_err(|_| "Request observation closed")?; },
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
            _ = liveness.tick() => {
                let status = administration.ai_runtime.refresh_status().await;
                publish(Update::RuntimeStatus(status));
                if let Some(workflow) = native_workflow.as_mut() {
                    let lost = workflow.refresh_worker().await.is_err();
                    publish_native(&native_workflow, publish);
                    if lost {
                        publish(Update::NativeError("Связь с нативным worker прервана".into()));
                    }
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
            let name = catalog
                .resolve_set(character, selected)
                .map_err(|error| error.to_string())?;
            catalog
                .load_post_rules(character, &name)
                .map_err(|error| error.to_string())?
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

pub fn data_directory() -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("NEUROMITA_DATA_DIR") {
        if directory.is_empty() {
            return Err("NEUROMITA_DATA_DIR must not be empty".into());
        }
        return Ok(directory.into());
    }
    #[cfg(windows)]
    if let Some(directory) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(directory).join("NeuroMita"));
    }
    #[cfg(not(windows))]
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(directory).join("neuromita"));
    }
    #[cfg(not(windows))]
    if let Some(directory) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(directory).join(".config/neuromita"));
    }
    Err("Specify --data-dir or NEUROMITA_DATA_DIR".into())
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
        let sink = DesktopTranscriptSink(tx);
        sink.reply(&Transcript {
            text: "spoken words".into(),
            language: None,
        })
        .await
        .unwrap();
        assert_eq!(rx.recv().await.unwrap().text, "spoken words");
    }
}
