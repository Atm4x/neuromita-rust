use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, thread, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    presentation::{SandboxPresenter, SandboxSnapshot},
    request::{CancelResult, RequestId},
    settings::Volume,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub enum Command {
    Connect(String),
    Select(CharacterId),
    Send(String),
    Cancel(RequestId),
    Refresh,
    Volume(u8),
}

pub enum Update {
    Stopped,
    Connecting,
    Connected(String),
    Snapshot(Box<SandboxSnapshot>),
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

async fn start(directory: &std::path::Path, model: &str) -> Result<ApplicationLifecycle, String> {
    let key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| "GEMINI_API_KEY is required in the application's environment".to_owned())?;
    let generation =
        GenerationSettings::new(0.7, 3000, Duration::from_secs(60)).map_err(|e| e.to_string())?;
    let template = GeminiTemplate::google(model, &key, generation).map_err(|e| e.to_string())?;
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
    let config = ApplicationConfig::persistent(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        generator,
        limits,
        "127.0.0.1:0".parse().unwrap(),
        directory,
    )
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
    let mut next_model = initial_model;
    loop {
        let model = match next_model.take() {
            Some(model) => model,
            None => loop {
                tokio::select! {
                    biased;
                    _ = stop.cancelled() => return,
                    command = receive.recv() => match command {
                        Some(Command::Connect(model)) => break model,
                        None => return,
                        _ => publish(Update::Error("Core is not connected".into())),
                    }
                }
            },
        };
        publish(Update::Connecting);
        let app = match start(&directory, &model).await {
            Ok(app) => app,
            Err(error) => {
                publish(Update::Error(error));
                continue;
            }
        };
        publish(Update::Connected(app.server().address().to_string()));
        let presenter = app.presenter().sandbox.clone();
        if let Err(error) = observe(&presenter, &mut receive, &stop, &publish).await {
            publish(Update::Error(error));
        }
        if let Err(error) = app.shutdown().await {
            publish(Update::Error(error.to_string()));
        }
        publish(Update::Stopped);
        return;
    }
}

async fn observe(
    presenter: &SandboxPresenter,
    receive: &mut mpsc::Receiver<Command>,
    stop: &CancellationToken,
    publish: &impl Fn(Update),
) -> Result<(), String> {
    let mut character = CharacterId::Mita;
    let mut requests = presenter.observe_requests();
    let mut settings = presenter.observe_settings().map_err(|e| e.to_string())?;
    let mut presets = presenter.observe_presets();
    let mut selection = presenter.observe_selection().map_err(|e| e.to_string())?;
    loop {
        requests.borrow_and_update();
        settings.borrow_and_update();
        presets.borrow_and_update();
        selection.borrow_and_update();
        match presenter.snapshot(character).await {
            Ok(snapshot) => publish(Update::Snapshot(Box::new(snapshot))),
            Err(error) => publish(Update::Error(error.to_string())),
        }
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            command = receive.recv() => match command {
                None => return Ok(()),
                Some(Command::Select(id)) => character = id,
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
                    Err(error) => publish(Update::Error(error.to_string())),
                },
                Some(Command::Connect(_)) => publish(Update::Error("Core is already running".into())),
            },
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
