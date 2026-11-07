mod settings_cli;
mod terminal;

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
    let logging = match neuromita::diagnostics::init() {
        Ok(logging) => logging,
        Err(error) => {
            eprintln!("Logging initialization failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::debug!("starting application");
    let result = run(&logging).await;
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
        neuromita::diagnostics::boundary(
            neuromita::diagnostics::Boundary::CliStart,
            neuromita::diagnostics::Outcome::Started,
        );
        return settings_cli::run(arguments.collect());
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
    let app = ApplicationLifecycle::start(config).await?;
    tracing::info!(address = %app.server().address(), "server listening");
    let result = terminal::run(&app.presenter().chat, options, app.finetune()).await;
    let stopped = app.shutdown().await;
    result?;
    stopped?;
    tracing::info!("application stopped");
    Ok(())
}
