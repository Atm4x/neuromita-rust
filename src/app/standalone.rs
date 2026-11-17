use crate::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
    },
    telegram_admin::TelegramAdmin,
    telegram_voice::DynamicTelegramVoice,
};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};

pub async fn start(
    directory: &std::path::Path,
    model: Option<&str>,
    telegram_voice: Arc<DynamicTelegramVoice>,
    telegram_admin: Arc<TelegramAdmin>,
) -> Result<ApplicationLifecycle, String> {
    tracing::debug!(directory = %directory.display(), "desktop application initialization started");
    let provider: Arc<dyn GenerationProvider> =
        Arc::new(GeminiProvider::new().map_err(|e| e.to_string())?);
    let generator = ResponseGenerator::new([
        provider,
        Arc::new(crate::generation::openai::OpenAiProvider::new().map_err(|e| e.to_string())?)
            as Arc<dyn GenerationProvider>,
    ])
    .map_err(|e| e.to_string())?;
    tracing::debug!("desktop generation providers initialized");
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
        crate::ai_runtime::installed_speech::InstalledSpeechEnvironment::for_desktop(
            directory.to_path_buf(),
        ),
    )
    .with_ai_hub_install_root(directory.to_path_buf());
    let prompts = crate::prompts::PromptCatalog::open_portable(directory).map_err(|error| {
        format!(
            "Cannot initialize {}: {error}",
            directory.join("Prompts").display()
        )
    })?;
    let mut config = config.with_prompt_catalog(prompts);
    config.video_capture = crate::video_capture::VideoCapture::for_desktop(directory);
    config.game_link = Some(crate::game_link::GameLinkConfig::new(
        "127.0.0.1:12345"
            .parse()
            .expect("valid Unity loopback endpoint"),
    ));
    config.voice = telegram_voice;
    config.telegram_admin = Some(telegram_admin);
    config.remote_voice_store = Some(directory.join("remote_voice.rust.json"));
    tracing::debug!("desktop application configuration prepared");
    ApplicationLifecycle::start(config)
        .await
        .map_err(|e| e.to_string())
}
