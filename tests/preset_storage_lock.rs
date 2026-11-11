use std::{num::NonZeroUsize, sync::Arc};

use neuromita::{
    app::{ApplicationConfig, RuntimeLimits},
    generation::{GenerationProvider, ResponseGenerator, gemini::GeminiProvider},
    settings::{PresetActivation, PresetActivationError, PresetRecovery},
};

struct TemporaryRoot(std::path::PathBuf);

impl TemporaryRoot {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("neuromita-preset-storage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(root: &TemporaryRoot) -> Result<ApplicationConfig, neuromita::app::ApplicationError> {
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let limits = RuntimeLimits {
        request_queue: NonZeroUsize::new(2).unwrap(),
        discovery_buffer: NonZeroUsize::new(8).unwrap(),
        character_commands: NonZeroUsize::new(8).unwrap(),
        presenter_updates: NonZeroUsize::new(8).unwrap(),
        retained_finished: NonZeroUsize::new(4).unwrap(),
    };
    ApplicationConfig::persistent_unconfigured(
        ResponseGenerator::new([provider]).unwrap(),
        limits,
        "127.0.0.1:0".parse().unwrap(),
        &root.0,
    )
}

#[test]
fn persistent_core_owns_exclusive_preset_lease_until_catalog_is_dropped() {
    let root = TemporaryRoot::new();
    let first = config(&root).unwrap();
    assert!(matches!(
        PresetActivation::recover(&root.0),
        Err(PresetActivationError::StorageBusy)
    ));
    let review = root.0.join("review");
    std::fs::create_dir(&review).unwrap();
    assert!(matches!(
        PresetActivation::apply(&review, &root.0),
        Err(PresetActivationError::StorageBusy)
    ));
    assert!(!root.0.join(".preset-activation-journal").exists());
    assert!(matches!(
        config(&root),
        Err(neuromita::app::ApplicationError::Settings(
            neuromita::settings::SettingsError::Storage(_)
        ))
    ));
    drop(first);
    assert_eq!(
        PresetActivation::recover(&root.0).unwrap(),
        PresetRecovery::NoJournal
    );
    let second = config(&root).unwrap();
    drop(second);
    assert!(root.0.join(".preset-storage.lock").is_file());
}

#[tokio::test]
async fn running_core_holds_lease_until_shutdown_completes() {
    use neuromita::app::ApplicationLifecycle;

    let root = TemporaryRoot::new();
    let core = ApplicationLifecycle::start(config(&root).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        PresetActivation::recover(&root.0),
        Err(PresetActivationError::StorageBusy)
    ));
    core.shutdown().await.unwrap();
    assert_eq!(
        PresetActivation::recover(&root.0).unwrap(),
        PresetRecovery::NoJournal
    );
}
