use std::{
    collections::HashMap, num::NonZeroUsize, path::PathBuf, sync::Arc, thread, time::Duration,
};

use neuromita::{
    ai_runtime::{
        AiService, fetch_speech_settings_schema,
        installed_speech::InstalledSpeechEnvironment,
        runtime_status::{RuntimePhase, RuntimeSnapshot},
        speech::Transcript,
        speech_catalog::{SpeechInventory, SpeechModelCatalog},
        speech_schema::{SpeechSettingsSchema, scalar_text},
    },
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    audio::{
        controller::{SpeechCaptureConfig, SpeechSessionSnapshot},
        input_catalog::{AudioInputCatalog, AudioInputDevice},
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
    UpdatePreset(PresetId, PresetRecord),
    DeletePreset(PresetId),
    ActivePreset(PresetId),
    AssignPreset(CharacterId, PresetSlot, PresetSelection),
    AssignAllPresets(CharacterId, PresetSelection),
    VoiceInLauncher(bool),
    VoiceInGame(bool),
    VoiceName(String),
    SaveTts(TtsPreferences),
    SaveFishAudioCredential(String),
    SaveCharacterVoice(Option<CharacterId>, String),
    SaveAsr(AsrPreferences, String),
    SelectAsrRecognizer(String),
    RefreshSpeechInventory,
    SaveSpeechOption(AiService, String, String, String),
    ResetSpeechOptions(AiService, String),
    RefreshHardware,
    SectionVisibility(neuromita::settings::SettingsArea, bool),
    HidePrivate(bool),
    Select(CharacterId),
    Send(String),
    Cancel(RequestId),
    Refresh,
    Volume(u8),
    SpeechSessionEnabled(bool),
    SpeechPushToTalk(bool),
}

pub struct SpeechOptionControl {
    pub key: String,
    pub kind: String,
    pub value: String,
    pub choices: Vec<String>,
}

pub enum Update {
    RuntimeStatus(RuntimeSnapshot),
    RemoteCredentialPresent(bool),
    SpeechModelSchema {
        service: AiService,
        plugin_id: String,
        status: String,
        fields: Vec<SpeechOptionControl>,
    },
    SpeechSessionStatus(SpeechSessionSnapshot),
    RecognizedText {
        text: String,
        auto_send_after: Option<Duration>,
        merge_with_draft: bool,
    },
    Hardware(neuromita::presentation::hardware::HardwareView),
    CharacterCatalog(Vec<neuromita::character::CharacterDefinition>),
    SpeechInventory(Box<SpeechInventory>, Vec<AudioInputDevice>),
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

fn schema_fields(
    schema: &SpeechSettingsSchema,
    options: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Vec<SpeechOptionControl> {
    let empty = serde_json::Map::new();
    let options = options.unwrap_or(&empty);
    schema
        .fields
        .iter()
        .filter(|field| field.key != "language")
        .map(|field| SpeechOptionControl {
            key: field.key.clone(),
            kind: field.kind.name().into(),
            value: field.display_value(options),
            choices: field.choices.iter().map(scalar_text).collect(),
        })
        .collect()
}

fn selected_model_for_schema(
    service: AiService,
    settings: &neuromita::settings::SpeechSettings,
) -> String {
    match service {
        AiService::Tts => settings.tts.local_model_id.clone(),
        AiService::Asr => settings.asr.recognizer_id.clone(),
        _ => String::new(),
    }
}

fn current_schema_values<'a>(
    service: AiService,
    speech: &'a neuromita::settings::SpeechSettings,
    plugin_id: &str,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    let values = match service {
        AiService::Tts => &speech.tts_model_options,
        AiService::Asr => &speech.asr_model_options,
        _ => return None,
    };
    values.get(plugin_id).and_then(serde_json::Value::as_object)
}

fn publish_speech_schema(
    service: AiService,
    plugin_id: String,
    schema: &SpeechSettingsSchema,
    speech: &neuromita::settings::SpeechSettings,
    publish: &impl Fn(Update),
) {
    let fields = schema_fields(schema, current_schema_values(service, speech, &plugin_id));
    publish(Update::SpeechModelSchema {
        service,
        plugin_id,
        fields,
        status: "Параметры из Python settings_schema; изменения сохраняются автоматически".into(),
    });
}

fn save_speech_option(
    api: &GuiPresenter,
    cache: &HashMap<AiService, (String, SpeechSettingsSchema)>,
    service: AiService,
    plugin_id: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let current = api.settings.snapshot().map_err(|error| error.to_string())?;
    if selected_model_for_schema(service, &current.values.speech) != plugin_id {
        return Err("Выбор модели изменился; настройка не применена".into());
    }
    let (_, schema) = cache
        .get(&service)
        .filter(|(id, _)| id == plugin_id)
        .ok_or("Схема выбранной модели ещё не загружена")?;
    if key == "language" {
        return Err("Язык задаётся отдельным параметром модели".into());
    }
    let field = schema
        .field(key)
        .ok_or("Неизвестный параметр выбранного плагина")?;
    let converted = field
        .parse_input(value)
        .map_err(|error| error.to_string())?;
    let model_options = match service {
        AiService::Tts => &api.settings.tts.model_options,
        AiService::Asr => &api.settings.asr.model_options,
        _ => return Err("Неподдерживаемый сервис".into()),
    };
    let mut values = model_options.get();
    let options = values
        .entry(plugin_id.to_owned())
        .or_insert_with(|| serde_json::json!({}));
    let map = options
        .as_object_mut()
        .ok_or("Некорректные сохранённые параметры модели")?;
    map.insert(key.to_owned(), converted);
    schema
        .validate_overrides(map)
        .map_err(|error| error.to_string())?;
    model_options.set(values).map_err(|error| error.to_string())
}

async fn discover_input_devices(directory: &std::path::Path) -> Vec<AudioInputDevice> {
    let configured = std::env::var_os("NEUROMITA_FFMPEG").map(PathBuf::from);
    let candidates = [
        configured,
        Some(directory.join("AIData/bin/ffmpeg.exe")),
        Some(directory.join("AIData/bin/ffmpeg")),
    ];
    let executable = candidates
        .into_iter()
        .flatten()
        .find(|path| path.is_absolute() && path.is_file());
    AudioInputCatalog::discover(executable.as_deref()).await
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
    let devices = discover_input_devices(directory).await;
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
    .ok_or("FFmpeg не найден; укажите абсолютный путь к исполняемому файлу")?;
    let ffplay = find_binary(
        directory,
        "NEUROMITA_FFPLAY",
        if cfg!(windows) {
            "ffplay.exe"
        } else {
            "ffplay"
        },
    )
    .ok_or("FFplay не найден; укажите абсолютный путь к исполняемому файлу")?;
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
    let generator = ResponseGenerator::new([provider]).map_err(|e| e.to_string())?;
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
    publish(Update::SpeechInventory(
        Box::new(catalog.snapshot().await),
        discover_input_devices(&directory).await,
    ));
    let app = match start(&directory, initial_model.as_deref()).await {
        Ok(app) => app,
        Err(error) => {
            publish(Update::Error(error));
            publish(Update::Stopped);
            return;
        }
    };
    publish(Update::Connected(app.server().address().to_string()));
    publish(Update::RemoteCredentialPresent(
        std::env::var_os("NEUROMITA_FISH_AUDIO_API_KEY").is_some()
            || directory.join("AIData/secrets/fish_audio.key").is_file(),
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
    let mut requests = presenter.observe_requests();
    let mut settings = presenter.observe_settings().map_err(|e| e.to_string())?;
    let mut presets = presenter.observe_presets();
    let mut selection = presenter.observe_selection().map_err(|e| e.to_string())?;
    let mut runtime_status = administration.ai_runtime.observe();
    let mut speech_status = administration.speech.observe();
    let (schema_tx, mut schema_rx) =
        mpsc::unbounded_channel::<(AiService, String, u64, Result<SpeechSettingsSchema, String>)>();
    let mut schema_requested = HashMap::<AiService, (String, u64)>::new();
    let mut next_schema_request = 0u64;
    let mut schema_cache = HashMap::<AiService, (String, SpeechSettingsSchema)>::new();
    let metadata = InstalledSpeechEnvironment::for_desktop(directory.to_path_buf());
    let (transcripts, mut transcript_updates) = mpsc::unbounded_channel();
    let transcript_sink: Arc<dyn SpeechConversation> = Arc::new(DesktopTranscriptSink(transcripts));
    let mut liveness = tokio::time::interval(Duration::from_secs(5));
    liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
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
                },
            ))),
            Err(error) => publish(Update::AdminError(error.to_string())),
        }
        match presenter.snapshot(character).await {
            Ok(snapshot) => publish(Update::Snapshot(Box::new(snapshot))),
            Err(error) => publish(Update::Error(error.to_string())),
        }
        if let Ok(snapshot) = administration.settings.snapshot() {
            for service in [AiService::Tts, AiService::Asr] {
                let plugin_id = selected_model_for_schema(service, &snapshot.values.speech);
                if !schema_requested
                    .get(&service)
                    .is_some_and(|(id, _)| id == &plugin_id)
                {
                    next_schema_request = next_schema_request.wrapping_add(1);
                    let generation = next_schema_request;
                    schema_requested.insert(service, (plugin_id.clone(), generation));
                    schema_cache.remove(&service);
                    publish(Update::SpeechModelSchema {
                        service,
                        plugin_id: plugin_id.clone(),
                        fields: vec![],
                        status: if plugin_id.is_empty() {
                            "Выберите модель".into()
                        } else {
                            "Чтение settings_schema из Python SDK…".into()
                        },
                    });
                    if !plugin_id.is_empty() {
                        let host = metadata.clone();
                        let send = schema_tx.clone();
                        tokio::spawn(async move {
                            let result = async {
                                let config = host
                                    .metadata_host(&plugin_id)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                fetch_speech_settings_schema(&config, service, &plugin_id)
                                    .await
                                    .map_err(|error| error.to_string())
                            }
                            .await;
                            let _ = send.send((service, plugin_id, generation, result));
                        });
                    }
                }
            }
        }
        publish(Update::RuntimeStatus(administration.ai_runtime.snapshot()));
        publish(Update::SpeechSessionStatus(
            administration.speech.snapshot(),
        ));
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            Some((service, plugin_id, generation, result)) = schema_rx.recv() => {
                if schema_requested.get(&service) != Some(&(plugin_id.clone(), generation)) { continue; }
                match result {
                    Ok(schema) => {
                        let snapshot = administration.settings.snapshot().map_err(|error| error.to_string())?;
                        publish_speech_schema(service, plugin_id.clone(), &schema, &snapshot.values.speech, publish);
                        schema_cache.insert(service, (plugin_id, schema));
                    },
                    Err(error) => publish(Update::SpeechModelSchema {
                        service, plugin_id, fields: vec![],
                        status: format!("Схема недоступна: {error}"),
                    }),
                }
            },
            command = receive.recv() => match command {
                Some(command @ Command::UpdatePreset(id, _)) => {
                    match super::settings::execute(administration, command) {
                        Ok(()) => publish(Update::PresetSaved(id)),
                        Err(error) => publish(Update::AdminError(error)),
                    }
                },
                Some(command @ (Command::DeletePreset(_) | Command::ActivePreset(_) | Command::AssignPreset(..) | Command::AssignAllPresets(..) | Command::VoiceInLauncher(_) | Command::VoiceInGame(_) | Command::VoiceName(_) | Command::SectionVisibility(..) | Command::HidePrivate(_) | Command::SaveTts(_) | Command::SaveAsr(..) | Command::SaveCharacterVoice(..) | Command::SelectAsrRecognizer(_))) => {
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
                Some(Command::ResetSpeechOptions(service, plugin_id)) => {
                    let snapshot = administration.settings.snapshot().map_err(|error| error.to_string())?;
                    if selected_model_for_schema(service, &snapshot.values.speech) != plugin_id {
                        publish(Update::AdminError("Модель изменилась; сброс отклонён".into()));
                    } else {
                        let options = match service {
                            AiService::Tts => Some(&administration.settings.tts.model_options),
                            AiService::Asr => Some(&administration.settings.asr.model_options),
                            _ => None,
                        };
                        if let Some(options) = options {
                            let mut values = options.get();
                            values.remove(&plugin_id);
                            match options.set(values) {
                                Ok(()) => {
                                    if let Some((_, schema)) = schema_cache.get(&service) {
                                        let current = administration.settings.snapshot().map_err(|error| error.to_string())?;
                                        publish_speech_schema(service, plugin_id, schema, &current.values.speech, publish);
                                    }
                                    publish(Update::AdminSaved);
                                },
                                Err(error) => publish(Update::AdminError(error.to_string())),
                            }
                        }
                    }
                },
                Some(Command::SaveFishAudioCredential(key)) => {
                    let file = directory.join("AIData/secrets/fish_audio.key");
                    match neuromita::adapters::fish_audio::save_key(&file, &key).await {
                        Ok(()) => publish(Update::RemoteCredentialPresent(true)),
                        Err(error) => publish(Update::AdminError(error.to_string())),
                    }
                },
                Some(Command::SaveSpeechOption(service, plugin_id, key, value)) => {
                    match save_speech_option(administration, &schema_cache, service, &plugin_id, &key, &value) {
                        Ok(()) => {
                            let speech = administration.settings.snapshot().map_err(|error| error.to_string())?;
                            if let Some((_, schema)) = schema_cache.get(&service) {
                                publish_speech_schema(service, plugin_id, schema, &speech.values.speech, publish);
                            }
                            publish(Update::AdminSaved);
                        },
                        Err(error) => {
                            publish(Update::AdminError(error));
                            if let (Some((cached_plugin, schema)), Ok(snapshot)) = (
                                schema_cache.get(&service),
                                administration.settings.snapshot(),
                            ) && cached_plugin == &plugin_id {
                                publish_speech_schema(
                                    service,
                                    plugin_id,
                                    schema,
                                    &snapshot.values.speech,
                                    publish,
                                );
                            }
                        },
                    }
                },
                Some(Command::RefreshHardware) => publish(hardware_snapshot().await),
                Some(Command::RefreshSpeechInventory) => {
                    schema_requested.clear();
                    publish(Update::SpeechInventory(Box::new(catalog.snapshot().await), discover_input_devices(&catalog.data_directory).await));
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
            },
        }
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
