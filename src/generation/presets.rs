use std::{
    collections::BTreeMap,
    fmt,
    io::Write,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;

use super::{
    GenerationPreset, ProviderTemplate,
    gemini::{GeminiTemplate, GenerationSettings},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PresetId(pub u32);

#[derive(Debug, Error)]
pub enum PresetCatalogError {
    #[error("preset catalog is unavailable")]
    Unavailable,
    #[error("preset {0:?} does not exist")]
    NotFound(PresetId),
    #[error("preset {0:?} is a runtime-only preset and cannot be edited")]
    RuntimeOnly(PresetId),
    #[error("preset identifier space is exhausted")]
    IdExhausted,
    #[error("invalid preset definition: {0}")]
    Invalid(String),
    #[error("credential reference is unavailable: {0}")]
    MissingCredential(String),
    #[error("preset storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeminiPresetDefinition {
    pub endpoint: String,
    pub model: String,
    /// Name of an environment variable; never the secret value itself.
    pub credential_env: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
}

impl GeminiPresetDefinition {
    fn validate(&self) -> Result<(), PresetCatalogError> {
        if self.credential_env.is_empty()
            || !self
                .credential_env
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || self.credential_env.as_bytes()[0].is_ascii_digit()
        {
            return Err(PresetCatalogError::Invalid(
                "invalid credential environment variable name".into(),
            ));
        }
        self.template("validation-key").map(|_| ())
    }

    fn template(&self, credential: &str) -> Result<GeminiTemplate, PresetCatalogError> {
        let settings = GenerationSettings::new(
            self.temperature,
            self.max_output_tokens,
            Duration::from_millis(self.timeout_ms),
        )
        .map_err(|err| PresetCatalogError::Invalid(err.to_string()))?;
        GeminiTemplate::new(&self.endpoint, &self.model, credential, settings)
            .map_err(|err| PresetCatalogError::Invalid(err.to_string()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", content = "configuration", rename_all = "snake_case")]
pub enum PresetDefinition {
    Gemini(GeminiPresetDefinition),
}

impl PresetDefinition {
    fn validate(&self) -> Result<(), PresetCatalogError> {
        match self {
            Self::Gemini(config) => config.validate(),
        }
    }

    fn materialize(
        &self,
        credentials: &dyn PresetCredentials,
    ) -> Result<GenerationPreset, PresetCatalogError> {
        let template = match self {
            Self::Gemini(config) => {
                let credential = credentials.get(&config.credential_env)?;
                ProviderTemplate::Gemini(config.template(&credential)?)
            }
        };
        Ok(GenerationPreset::new(template))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRecord {
    pub name: String,
    pub definition: PresetDefinition,
}

impl PresetRecord {
    fn validate(&self) -> Result<(), PresetCatalogError> {
        if self.name.trim().is_empty() {
            return Err(PresetCatalogError::Invalid(
                "preset name must not be empty".into(),
            ));
        }
        self.definition.validate()
    }
}

pub trait PresetCredentials: Send + Sync {
    fn get(&self, key: &str) -> Result<String, PresetCatalogError>;
}

pub struct EnvironmentPresetCredentials;

impl PresetCredentials for EnvironmentPresetCredentials {
    fn get(&self, key: &str) -> Result<String, PresetCatalogError> {
        std::env::var(key)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PresetCatalogError::MissingCredential(key.into()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPresets {
    schema_version: u32,
    next_id: u32,
    entries: BTreeMap<PresetId, PresetRecord>,
}

impl StoredPresets {
    fn empty(next_id: u32) -> Self {
        Self {
            schema_version: 1,
            next_id,
            entries: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<(), PresetCatalogError> {
        if self.schema_version != 1 {
            return Err(PresetCatalogError::Storage(
                "unsupported preset catalog schema".into(),
            ));
        }
        if self.entries.keys().any(|id| id.0 >= self.next_id) {
            return Err(PresetCatalogError::Storage(
                "invalid preset identifier sequence".into(),
            ));
        }
        for value in self.entries.values() {
            value.validate()?;
        }
        Ok(())
    }
}

pub trait PresetRepository: Send + Sync {
    fn load(&self) -> Result<Option<StoredPresets>, PresetCatalogError>;
    fn save(&self, value: &StoredPresets) -> Result<(), PresetCatalogError>;
}

pub struct JsonPresetRepository {
    path: PathBuf,
}

impl JsonPresetRepository {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl PresetRepository for JsonPresetRepository {
    fn load(&self) -> Result<Option<StoredPresets>, PresetCatalogError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(PresetCatalogError::Storage(error.to_string())),
        };
        let stored: StoredPresets = serde_json::from_slice(&bytes)
            .map_err(|error| PresetCatalogError::Storage(error.to_string()))?;
        stored.validate()?;
        Ok(Some(stored))
    }

    fn save(&self, value: &StoredPresets) -> Result<(), PresetCatalogError> {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| PresetCatalogError::Storage(error.to_string()))?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> Result<(), std::io::Error> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|error| PresetCatalogError::Storage(error.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct PresetCatalogSnapshot {
    pub revision: u64,
    pub entries: BTreeMap<PresetId, PresetRecord>,
}

struct CatalogState {
    runtime: BTreeMap<PresetId, GenerationPreset>,
    stored: StoredPresets,
    revision: u64,
}

pub struct PresetCatalog {
    state: RwLock<CatalogState>,
    repository: Option<Arc<dyn PresetRepository>>,
    credentials: Arc<dyn PresetCredentials>,
    changes: watch::Sender<Arc<PresetCatalogSnapshot>>,
}

impl fmt::Debug for PresetCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresetCatalog").finish_non_exhaustive()
    }
}

impl PresetCatalog {
    pub fn new(entries: BTreeMap<PresetId, GenerationPreset>) -> Self {
        let next_id = entries
            .keys()
            .next_back()
            .and_then(|id| id.0.checked_add(1))
            .unwrap_or(1);
        let stored = StoredPresets::empty(next_id);
        let (changes, _) = watch::channel(Arc::new(PresetCatalogSnapshot {
            revision: 0,
            entries: stored.entries.clone(),
        }));
        Self {
            state: RwLock::new(CatalogState {
                runtime: entries,
                stored,
                revision: 0,
            }),
            repository: None,
            credentials: Arc::new(EnvironmentPresetCredentials),
            changes,
        }
    }

    pub fn single(preset: GenerationPreset) -> Self {
        Self::new(BTreeMap::from([(PresetId(0), preset)]))
    }

    pub fn open(
        runtime: BTreeMap<PresetId, GenerationPreset>,
        repository: Arc<dyn PresetRepository>,
        credentials: Arc<dyn PresetCredentials>,
    ) -> Result<Self, PresetCatalogError> {
        let min_next = runtime
            .keys()
            .next_back()
            .map_or(0, |id| id.0.saturating_add(1));
        let stored = repository
            .load()?
            .unwrap_or_else(|| StoredPresets::empty(min_next));
        stored.validate()?;
        if stored.entries.keys().any(|id| runtime.contains_key(id)) {
            return Err(PresetCatalogError::Storage(
                "persisted ID conflicts with runtime preset".into(),
            ));
        }
        if stored.next_id < min_next {
            return Err(PresetCatalogError::Storage(
                "persisted catalog overlaps reserved runtime IDs".into(),
            ));
        }
        let (changes, _) = watch::channel(Arc::new(PresetCatalogSnapshot {
            revision: 0,
            entries: stored.entries.clone(),
        }));
        Ok(Self {
            state: RwLock::new(CatalogState {
                runtime,
                stored,
                revision: 0,
            }),
            repository: Some(repository),
            credentials,
            changes,
        })
    }

    pub fn get(&self, id: PresetId) -> Result<Option<GenerationPreset>, PresetCatalogError> {
        let state = self.state.read().expect("preset catalog poisoned");
        if let Some(preset) = state.runtime.get(&id) {
            return Ok(Some(preset.clone()));
        }
        let record = state.stored.entries.get(&id).cloned();
        drop(state);
        record
            .map(|entry| entry.definition.materialize(self.credentials.as_ref()))
            .transpose()
    }

    pub fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.changes.subscribe()
    }

    pub fn list(&self) -> Vec<(PresetId, PresetRecord)> {
        self.state
            .read()
            .expect("preset catalog poisoned")
            .stored
            .entries
            .iter()
            .map(|(id, value)| (*id, value.clone()))
            .collect()
    }

    pub fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        record.validate()?;
        let mut state = self.state.write().expect("preset catalog poisoned");
        let id = PresetId(state.stored.next_id);
        let next_id = id.0.checked_add(1).ok_or(PresetCatalogError::IdExhausted)?;
        let mut next = state.stored.clone();
        next.next_id = next_id;
        next.entries.insert(id, record);
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(id)
    }

    pub fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        record.validate()?;
        let mut state = self.state.write().expect("preset catalog poisoned");
        if state.runtime.contains_key(&id) {
            return Err(PresetCatalogError::RuntimeOnly(id));
        }
        if !state.stored.entries.contains_key(&id) {
            return Err(PresetCatalogError::NotFound(id));
        }
        let mut next = state.stored.clone();
        next.entries.insert(id, record);
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(())
    }

    pub fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        let mut state = self.state.write().expect("preset catalog poisoned");
        if state.runtime.contains_key(&id) {
            return Err(PresetCatalogError::RuntimeOnly(id));
        }
        let mut next = state.stored.clone();
        if next.entries.remove(&id).is_none() {
            return Err(PresetCatalogError::NotFound(id));
        }
        self.persist(&next)?;
        self.publish(&mut state, next);
        Ok(())
    }

    fn publish(&self, state: &mut CatalogState, stored: StoredPresets) {
        state.revision += 1;
        state.stored = stored;
        self.changes.send_replace(Arc::new(PresetCatalogSnapshot {
            revision: state.revision,
            entries: state.stored.entries.clone(),
        }));
    }

    fn persist(&self, value: &StoredPresets) -> Result<(), PresetCatalogError> {
        if let Some(repo) = &self.repository {
            repo.save(value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestCredentials(Mutex<BTreeMap<String, String>>);
    impl PresetCredentials for TestCredentials {
        fn get(&self, key: &str) -> Result<String, PresetCatalogError> {
            self.0
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| PresetCatalogError::MissingCredential(key.into()))
        }
    }

    fn example(model: &str) -> PresetRecord {
        PresetRecord {
            name: model.into(),
            definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                endpoint: "http://127.0.0.1:8888/v1beta/".into(),
                model: model.into(),
                credential_env: "MY_TEST_API_KEY".into(),
                temperature: 0.7,
                max_output_tokens: 200,
                timeout_ms: 4000,
            }),
        }
    }

    #[test]
    fn catalog_crud_reopen_and_secret_free_storage() {
        let path =
            std::env::temp_dir().join(format!("neuromita-presets-{}.json", uuid::Uuid::new_v4()));
        let credentials = Arc::new(TestCredentials(Mutex::new(BTreeMap::from([(
            "MY_TEST_API_KEY".into(),
            "private-test-key".into(),
        )]))));
        let store: Arc<dyn PresetRepository> = Arc::new(JsonPresetRepository::new(path.clone()));
        let catalog =
            PresetCatalog::open(BTreeMap::new(), Arc::clone(&store), credentials.clone()).unwrap();
        let id = catalog.create(example("alpha")).unwrap();
        assert_eq!(id, PresetId(0));
        assert_eq!(catalog.list().len(), 1);
        assert!(format!("{:?}", catalog.get(id).unwrap().unwrap()).contains("alpha"));
        let prepared = catalog.get(id).unwrap().unwrap();
        catalog.update(id, example("beta")).unwrap();
        assert!(format!("{prepared:?}").contains("alpha"));
        assert!(format!("{:?}", catalog.get(id).unwrap().unwrap()).contains("beta"));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("private-test-key"));
        assert!(saved.contains("MY_TEST_API_KEY"));
        let reopened = PresetCatalog::open(BTreeMap::new(), store, credentials).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert!(format!("{:?}", reopened.get(id).unwrap().unwrap()).contains("beta"));
        reopened.delete(id).unwrap();
        assert!(reopened.get(id).unwrap().is_none());
        let next_id = reopened.create(example("gamma")).unwrap();
        assert_ne!(next_id, id);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_credentials_and_invalid_configs_are_explicit() {
        let credentials = Arc::new(TestCredentials(Mutex::new(BTreeMap::new())));
        let store: Arc<dyn PresetRepository> = Arc::new(MemoryPresetRepository::default());
        let catalog = PresetCatalog::open(BTreeMap::new(), store, credentials).unwrap();
        let id = catalog.create(example("model")).unwrap();
        assert!(matches!(
            catalog.get(id),
            Err(PresetCatalogError::MissingCredential(_))
        ));
        let mut invalid = example("bad/model");
        assert!(catalog.create(invalid.clone()).is_err());
        let PresetDefinition::Gemini(config) = &mut invalid.definition;
        config.model = "good".into();
        config.credential_env = "BAD-ENV".into();
        assert!(catalog.update(id, invalid).is_err());
        assert_eq!(catalog.list().len(), 1);
    }

    #[test]
    fn failed_persistence_does_not_publish_a_preset_or_notify_observers() {
        let directory = std::env::temp_dir().join(format!(
            "neuromita-no-preset-directory-{}",
            uuid::Uuid::new_v4()
        ));
        let catalog = PresetCatalog::open(
            BTreeMap::new(),
            Arc::new(JsonPresetRepository::new(directory.join("presets.json"))),
            Arc::new(TestCredentials(Mutex::new(BTreeMap::new()))),
        )
        .unwrap();
        let observer = catalog.observe();
        assert!(matches!(
            catalog.create(example("alpha")),
            Err(PresetCatalogError::Storage(_))
        ));
        assert!(catalog.list().is_empty());
        assert_eq!(observer.borrow().revision, 0);
        assert!(!observer.has_changed().unwrap());
    }

    #[derive(Default)]
    struct MemoryPresetRepository(Mutex<Option<StoredPresets>>);
    impl PresetRepository for MemoryPresetRepository {
        fn load(&self) -> Result<Option<StoredPresets>, PresetCatalogError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, value: &StoredPresets) -> Result<(), PresetCatalogError> {
            *self.0.lock().unwrap() = Some(value.clone());
            Ok(())
        }
    }
}
