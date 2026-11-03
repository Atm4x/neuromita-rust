use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, thread, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
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
    settings::{PresetId, PresetSelection, Volume},
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
    CreatePreset(PresetRecord, uuid::Uuid),
    UpdatePreset(PresetId, PresetRecord),
    DeletePreset(PresetId),
    ActivePreset(PresetId),
    AssignPreset(CharacterId, PresetSlot, PresetSelection),
    AssignAllPresets(CharacterId, PresetSelection),
    VoiceInLauncher(bool),
    VoiceInGame(bool),
    VoiceName(String),
    SectionVisibility(usize, bool),
    HidePrivate(bool),
    Select(CharacterId),
    Send(String),
    Cancel(RequestId),
    Refresh,
    Volume(u8),
}

pub enum Update {
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
    AdminSaved(Option<(PresetId, uuid::Uuid)>),
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
    .map_err(|e| e.to_string())?;
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
    let app = match start(&directory, initial_model.as_deref()).await {
        Ok(app) => app,
        Err(error) => {
            publish(Update::Error(error));
            publish(Update::Stopped);
            return;
        }
    };
    publish(Update::Connected(app.server().address().to_string()));
    if let Err(error) = observe(app.presenter(), &mut receive, &stop, &publish).await {
        publish(Update::Error(error));
    }
    if let Err(error) = app.shutdown().await {
        publish(Update::Error(error.to_string()));
    }
    publish(Update::Stopped);
}

async fn observe(
    administration: &GuiPresenter,
    receive: &mut mpsc::Receiver<Command>,
    stop: &CancellationToken,
    publish: &impl Fn(Update),
) -> Result<(), String> {
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
                    presets: administration.presets.list(),
                },
            ))),
            Err(error) => publish(Update::AdminError(error.to_string())),
        }
        match presenter.snapshot(character).await {
            Ok(snapshot) => publish(Update::Snapshot(Box::new(snapshot))),
            Err(error) => publish(Update::Error(error.to_string())),
        }
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            command = receive.recv() => match command {
                Some(command @ (Command::CreatePreset(..) | Command::UpdatePreset(..) | Command::DeletePreset(_) | Command::ActivePreset(_) | Command::AssignPreset(..) | Command::AssignAllPresets(..) | Command::VoiceInLauncher(_) | Command::VoiceInGame(_) | Command::VoiceName(_) | Command::SectionVisibility(..) | Command::HidePrivate(_))) => {
                    match super::settings::execute(administration, command) {
                        Ok(id) => publish(Update::AdminSaved(id)),
                        Err(error) => publish(Update::AdminError(error)),
                    }
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
                        Some(trace) => format!("Request: {}\nPreset: {}\nContext: {:?}\nTransitions:\n{}", trace.metadata.id, trace.preset_description, trace.context.as_ref().map(|context| context.anchor), trace.transitions.iter().map(|step| format!("#{} {:?} · {:?}", step.revision, step.phase, step.state)).collect::<Vec<_>>().join("\n")),
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
            update = memories.updates.recv(), if !memories.closed => { match update { Some(update) => publish(update), None => memories.closed = true } },
            result = releases.changed() => { result.map_err(|_| "Release observation closed")?; },
            result = connections.changed() => { result.map_err(|_| "Connection observation closed")?; },
            result = requests.changed() => { result.map_err(|_| "Request observation closed")?; },
            result = settings.changed() => { result.map_err(|_| "Settings observation closed")?; },
            result = presets.changed() => { result.map_err(|_| "Preset observation closed")?; },
            result = selection.changed() => { result.map_err(|_| "Timeline observation closed")?; },
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
