#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli_tools;
#[cfg(feature = "gui")]
#[path = "bin/neuromita-gui/main.rs"]
mod desktop;
mod documents_cli;
mod documents_operator;
#[cfg(feature = "sqlite-system")]
mod recovery_inventory_cli;
#[cfg(feature = "sqlite-system")]
mod recovery_snapshot_cli;
mod settings_cli;
mod terminal;
#[cfg(feature = "sqlite-system")]
mod timeline_backup_cli;
#[cfg(feature = "sqlite-system")]
mod timeline_index_cli;
#[cfg(feature = "sqlite-system")]
mod timeline_restore_cli;

use std::{error::Error, num::NonZeroUsize, process::ExitCode, sync::Arc, time::Duration};

use neuromita::{
    adapters::gemini_embedding::{GeminiEmbeddingProtocol, GeminiEmbeddingProvider},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    knowledge::{
        KnowledgeRetrieval, embedding::EmbeddingProvider, semantic_memory::SemanticMemoryRetriever,
    },
};

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|argument| argument == "--version")
    {
        println!("NeuroMita {}", neuromita::version::VERSION);
        return ExitCode::SUCCESS;
    }
    #[cfg(feature = "gui")]
    if !arguments.iter().any(|argument| argument == "--headless")
        && (arguments.is_empty()
            || arguments.first().is_some_and(|argument| {
                matches!(
                    argument.as_str(),
                    "--data-dir"
                        | "--language"
                        | "--sandbox"
                        | "--voice-preview"
                        | "--asr-preview"
                        | "--unity-dir"
                        | "--launcher-config"
                )
            }))
    {
        return match desktop::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
    }
    let service = std::thread::Builder::new()
        .name("neuromita-core".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(Box::pin(run_cli(arguments))),
                Err(error) => fail(error),
            }
        });
    match service {
        Ok(service) => service
            .join()
            .unwrap_or_else(|_| fail("application service thread panicked")),
        Err(error) => fail(error),
    }
}

async fn run_cli(arguments: Vec<String>) -> ExitCode {
    if arguments.iter().any(|argument| argument == "--headless") {
        return match Box::pin(run_headless(&arguments)).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
    }
    let operation: Result<(), Box<dyn Error>> = match arguments.first().map(String::as_str) {
        #[cfg(feature = "mcp")]
        Some("mcp-stdio") => Box::pin(neuromita::mcp::run_stdio()).await,
        Some("agent") => Box::pin(neuromita::agent_ipc::run(arguments.into_iter().skip(1)))
            .await
            .map_err(Into::into),
        Some("agent-http") => Box::pin(cli_tools::agent_http::main())
            .await
            .map_err(Into::into),
        Some("i18n") => cli_tools::i18n::main(),
        Some("beat-sync") => {
            cli_tools::beat_sync::main();
            Ok(())
        }
        Some("semantic-benchmark") => Box::pin(cli_tools::semantic_benchmark::main()).await,
        Some("telegram-voice-smoke") => {
            Box::pin(cli_tools::telegram_voice_smoke::main()).await;
            Ok(())
        }
        Some(
            "--internal-apply-update"
            | "--auto-recover-interrupted-update"
            | "--recover-interrupted-update",
        ) => {
            neuromita::updater::handoff::run_helper(std::env::args_os().skip(1)).map_err(Into::into)
        }
        _ => return Box::pin(run_main()).await,
    };
    match operation {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

async fn run_headless(arguments: &[String]) -> Result<(), Box<dyn Error>> {
    let mut directory = None;
    let mut parameters = arguments.iter();
    while let Some(argument) = parameters.next() {
        match argument.as_str() {
            "--headless" => {}
            "--data-dir" => {
                directory = Some(std::path::PathBuf::from(
                    parameters.next().ok_or("--data-dir requires a path")?,
                ))
            }
            _ => return Err(format!("unsupported headless argument: {argument}").into()),
        }
    }
    let logging = neuromita::diagnostics::init().map_err(std::io::Error::other)?;
    let root = neuromita::portable::PortableDataRoot::resolve(directory.as_deref())?;
    root.ensure_writable()?;
    logging.attach(&root)?;
    let voice = Arc::new(neuromita::telegram_voice::DynamicTelegramVoice::default());
    let telegram = Arc::new(neuromita::telegram_admin::TelegramAdmin::new(
        root.path().to_owned(),
        voice.clone(),
    ));
    let app = Box::pin(neuromita::app::standalone::start(
        root.path(),
        None,
        voice,
        telegram,
    ))
    .await
    .map_err(std::io::Error::other)?;
    println!(
        "NeuroMita {} headless; Core {}",
        neuromita::version::VERSION,
        app.server().address()
    );
    tokio::signal::ctrl_c().await?;
    app.shutdown().await?;
    Ok(())
}

async fn run_main() -> ExitCode {
    let logging = match neuromita::diagnostics::init() {
        Ok(logging) => logging,
        Err(error) => {
            eprintln!("Logging initialization failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::debug!("starting application");
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let command = arguments.next();
    if command.as_deref() == Some(std::ffi::OsStr::new("documents")) {
        return match documents_cli::run(arguments.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("recovery-snapshot")) {
        #[cfg(feature = "sqlite-system")]
        return match recovery_snapshot_cli::run(arguments.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
        #[cfg(not(feature = "sqlite-system"))]
        return fail("recovery-snapshot requires the sqlite-system feature");
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("recovery-inventory")) {
        #[cfg(feature = "sqlite-system")]
        return match recovery_inventory_cli::run(arguments.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
        #[cfg(not(feature = "sqlite-system"))]
        return fail("recovery-inventory requires the sqlite-system feature");
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("timeline-backup")) {
        #[cfg(feature = "sqlite-system")]
        return match timeline_backup_cli::run(arguments.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
        #[cfg(not(feature = "sqlite-system"))]
        return fail("timeline-backup requires the sqlite-system feature");
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("timeline-restore")) {
        #[cfg(feature = "sqlite-system")]
        return match timeline_restore_cli::run(arguments.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
        #[cfg(not(feature = "sqlite-system"))]
        return fail("timeline-restore requires the sqlite-system feature");
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("timeline-index")) {
        #[cfg(feature = "sqlite-system")]
        return match timeline_index_cli::run(arguments.collect()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
        #[cfg(not(feature = "sqlite-system"))]
        return fail("timeline-index maintenance requires the sqlite-system feature");
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("dataset")) {
        let dataset = match arguments
            .map(|argument| {
                argument.into_string().map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "dataset arguments must be valid Unicode",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(arguments) => arguments,
            Err(error) => return fail(error),
        };
        return match neuromita::finetune::cli::run(dataset) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(error),
        };
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("launcher")) {
        return match neuromita::launcher::run_cli(arguments.collect::<Vec<_>>()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(error = %error, "application failed");
                ExitCode::FAILURE
            }
        };
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("hardware")) {
        return match neuromita::ai_runtime::hardware::run_cli(arguments.collect::<Vec<_>>()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(error = %error, "application failed");
                ExitCode::FAILURE
            }
        };
    }
    let result = Box::pin(run(&logging)).await;
    neuromita::diagnostics::boundary(
        neuromita::diagnostics::Boundary::CliFinish,
        if result.is_ok() {
            neuromita::diagnostics::Outcome::Completed
        } else {
            neuromita::diagnostics::Outcome::Failed
        },
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "application failed");
            ExitCode::FAILURE
        }
    }
}

fn fail(error: impl std::fmt::Display) -> ExitCode {
    tracing::error!(error = %error, "application failed");
    eprintln!("error: {error}");
    ExitCode::FAILURE
}

async fn run(logging: &neuromita::diagnostics::DiagnosticBootstrap) -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next();
    if matches!(command.as_deref(), Some(command) if command == "dataset" || command == "launcher" || command == "hardware")
    {
        let root = neuromita::portable::PortableDataRoot::resolve(None)?;
        logging.attach(&root)?;
        neuromita::diagnostics::boundary(
            neuromita::diagnostics::Boundary::CliStart,
            neuromita::diagnostics::Outcome::Started,
        );
        if command.as_deref() == Some(std::ffi::OsStr::new("dataset")) {
            let parameters = arguments
                .map(|argument| {
                    argument.into_string().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "dataset arguments must be valid Unicode",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            neuromita::finetune::cli::run(parameters)?;
        } else if command.as_deref() == Some(std::ffi::OsStr::new("launcher")) {
            neuromita::launcher::run_cli(arguments.collect())?;
        } else {
            neuromita::ai_runtime::hardware::run_cli(arguments.collect()).await?;
        }
        return Ok(());
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("settings")) {
        let parameters = arguments.collect::<Vec<_>>();
        if parameters.first().is_some_and(|value| value == "--help")
            && std::env::var_os("NEUROMITA_DATA_DIR").is_some()
        {
            let root = neuromita::portable::PortableDataRoot::resolve(None)?;
            logging.attach(&root)?;
        }
        neuromita::diagnostics::boundary(
            neuromita::diagnostics::Boundary::CliStart,
            neuromita::diagnostics::Outcome::Started,
        );
        return settings_cli::run(parameters);
    }
    let Some(options) = terminal::Options::parse()? else {
        return Ok(());
    };
    let root = neuromita::portable::PortableDataRoot::resolve(Some(&options.data_dir))?;
    logging.attach(&root)?;
    neuromita::diagnostics::boundary(
        neuromita::diagnostics::Boundary::CliStart,
        neuromita::diagnostics::Outcome::Started,
    );
    let key = std::env::var("GEMINI_API_KEY").map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "GEMINI_API_KEY must be set",
        )
    })?;
    let settings = GenerationSettings::new(0.7, 2048, Duration::from_secs(60))?;
    let template = GeminiTemplate::google(&options.model, &key, settings)?;
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(template));
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new()?);
    let generator = ResponseGenerator::new([
        provider,
        Arc::new(neuromita::generation::openai::OpenAiProvider::new().map_err(|e| e.to_string())?)
            as Arc<dyn GenerationProvider>,
    ])?;
    let mut knowledge = KnowledgeRetrieval::lexical();
    if let Some(model) = std::env::var_os("NEUROMITA_MEMORY_EMBEDDING_MODEL") {
        let protocol = match model.to_str() {
            Some("gemini-embedding-001") => GeminiEmbeddingProtocol::Embedding001,
            Some("gemini-embedding-2") => GeminiEmbeddingProtocol::Embedding2,
            _ => return Err("NEUROMITA_MEMORY_EMBEDDING_MODEL must name gemini-embedding-001 or gemini-embedding-2".into()),
        };
        let embedding: Arc<dyn EmbeddingProvider> =
            Arc::new(GeminiEmbeddingProvider::google(protocol, &key, 768)?);
        knowledge =
            KnowledgeRetrieval::semantic(Arc::new(SemanticMemoryRetriever::new(embedding, 512)?));
    }
    let limits = RuntimeLimits {
        request_queue: NonZeroUsize::new(16).expect("positive limit"),
        discovery_buffer: NonZeroUsize::new(128).expect("positive limit"),
        character_commands: NonZeroUsize::new(32).expect("positive limit"),
        presenter_updates: NonZeroUsize::new(128).expect("positive limit"),
        retained_finished: NonZeroUsize::new(100).expect("positive limit"),
    };
    let mut config = ApplicationConfig::persistent(
        preset,
        generator,
        limits,
        options.listen,
        &options.data_dir,
    )?;
    if let Some(path) = options.finetune_config.as_ref() {
        config.finetune = Some(neuromita::finetune::FineTuneSettings::load(path)?);
    }
    config.knowledge = knowledge;
    if options.installed_speech {
        let directory = if options.data_dir.is_absolute() {
            options.data_dir.clone()
        } else {
            std::env::current_dir()?.join(&options.data_dir)
        };
        config = config.with_installed_speech_runtime(
            neuromita::ai_runtime::installed_speech::InstalledSpeechEnvironment::for_desktop(
                directory,
            ),
        );
    }
    let app = ApplicationLifecycle::start(config).await?;
    if let Some(warning) = app.speech_startup_warning() {
        eprintln!("Installed speech activation failed: {warning}");
    }
    tracing::info!(address = %app.server().address(), "server listening");
    let result = terminal::run(&app, options, app.finetune()).await;
    let stopped = app.shutdown().await;
    result?;
    stopped?;
    tracing::info!("application stopped");
    Ok(())
}
