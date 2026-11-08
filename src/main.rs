mod logging;
#[cfg(feature = "sqlite-system")]
mod recovery_inventory_cli;
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

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = logging::init() {
        eprintln!("Logging initialization failed: {error}");
        return ExitCode::FAILURE;
    }
    tracing::debug!("starting application");
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let command = arguments.next();
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
    match run().await {
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

async fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new("settings")) {
        return settings_cli::run(arguments.collect());
    }
    let Some(options) = terminal::Options::parse()? else {
        return Ok(());
    };
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
