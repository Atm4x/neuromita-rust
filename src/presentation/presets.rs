use std::sync::Arc;

use tokio::sync::watch;

use crate::generation::{
    GeminiPresetDefinition, PresetCatalog, PresetCatalogError, PresetCatalogSnapshot,
    PresetDefinition, PresetId, PresetRecord, gemini::GeminiTemplate,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderOption {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub schema_label: &'static str,
    pub documentation: &'static str,
    pub badge: &'static str,
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetView {
    pub id: PresetId,
    pub name: String,
    pub model: String,
    pub provider: &'static str,
    pub endpoint: String,
    pub credential_env: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PresetsView {
    pub revision: u64,
    pub records: Vec<PresetView>,
    pub providers: Vec<ProviderOption>,
}

pub trait PresetsApi: Send + Sync {
    fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>>;
    fn snapshot(&self) -> PresetsView;
    fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError>;
    fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError>;
    fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError>;
    fn require(&self, id: PresetId) -> Result<(), PresetCatalogError>;
}

pub struct PresetsPresenter {
    catalog: Arc<PresetCatalog>,
}

impl PresetsPresenter {
    pub(crate) fn new(catalog: Arc<PresetCatalog>) -> Self {
        Self { catalog }
    }

    pub fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.catalog.observe()
    }

    pub fn list(&self) -> Vec<(PresetId, PresetRecord)> {
        self.catalog.list()
    }

    pub fn snapshot(&self) -> PresetsView {
        let catalog = self.catalog.observe();
        let snapshot = catalog.borrow();
        let records = snapshot
            .entries
            .iter()
            .map(|(&id, record)| {
                let PresetDefinition::Gemini(definition) = &record.definition;
                PresetView {
                    id,
                    name: record.name.clone(),
                    model: definition.model.clone(),
                    provider: "google",
                    endpoint: definition.endpoint.clone(),
                    credential_env: definition.credential_env.clone(),
                    temperature: definition.temperature,
                    max_output_tokens: definition.max_output_tokens,
                    timeout_ms: definition.timeout_ms,
                }
            })
            .collect();
        PresetsView {
            revision: snapshot.revision,
            records,
            providers: Self::providers(),
        }
    }

    pub fn providers() -> Vec<ProviderOption> {
        const CATALOG: &[(&str, &str, &str, &str, &str, bool)] = &[
            (
                "gemini",
                "Google AI Studio",
                "google",
                "https://ai.google.dev/gemini-api/docs",
                "FREE",
                true,
            ),
            (
                "openrouter",
                "OpenRouter",
                "openrouter",
                "https://openrouter.ai/docs/quickstart",
                "FREE",
                false,
            ),
            (
                "mistral",
                "Mistral AI",
                "mistral",
                "https://docs.mistral.ai/",
                "FREE",
                false,
            ),
            ("aiio", "Ai.iO", "aiio", "", "", false),
            ("proxyapi", "ProxyAPI", "proxyapi", "", "", false),
            (
                "groq",
                "Groq",
                "groq",
                "https://console.groq.com/docs/overview",
                "FREE",
                false,
            ),
            (
                "together",
                "Together AI",
                "together",
                "https://docs.together.ai/",
                "FREE",
                false,
            ),
            ("chutes", "Chutes", "chutes", "", "", false),
            ("kodikrouter", "KodikRouter", "kodikrouter", "", "", false),
            (
                "lmstudio",
                "LM Studio",
                "lmstudio",
                "https://lmstudio.ai/docs",
                "LOCAL",
                false,
            ),
            (
                "ollama",
                "Ollama",
                "ollama",
                "https://docs.ollama.com/",
                "LOCAL",
                false,
            ),
            (
                "openai",
                "OpenAI Responses (API key)",
                "openai",
                "https://platform.openai.com/docs/api-reference/responses",
                "",
                false,
            ),
            ("codex", "ChatGPT Plan (Codex)", "openai", "", "", false),
        ];
        CATALOG
            .iter()
            .map(
                |&(id, name, icon, documentation, badge, supported)| ProviderOption {
                    id,
                    name,
                    icon,
                    schema_label: if id == "gemini" {
                        "Google Gemini API"
                    } else {
                        ""
                    },
                    documentation,
                    badge,
                    supported,
                },
            )
            .collect()
    }

    pub fn new_draft(&self) -> PresetRecord {
        PresetRecord {
            name: String::new(),
            definition: PresetDefinition::Gemini(GeminiPresetDefinition {
                endpoint: GeminiTemplate::GOOGLE_ENDPOINT.into(),
                model: String::new(),
                credential_env: "GEMINI_API_KEY".into(),
                temperature: 0.7,
                max_output_tokens: 3000,
                timeout_ms: 60_000,
            }),
        }
    }

    pub fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        self.catalog.create(record)
    }

    pub fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        self.catalog.update(id, record)
    }

    pub fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.catalog.delete(id)
    }

    pub fn require(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        if self.catalog.contains(id) {
            Ok(())
        } else {
            Err(PresetCatalogError::NotFound(id))
        }
    }
}

impl PresetsApi for PresetsPresenter {
    fn observe(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.observe()
    }
    fn snapshot(&self) -> PresetsView {
        self.snapshot()
    }
    fn create(&self, record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        self.create(record)
    }
    fn update(&self, id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        self.update(id, record)
    }
    fn delete(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.delete(id)
    }
    fn require(&self, id: PresetId) -> Result<(), PresetCatalogError> {
        self.require(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_catalog_does_not_invent_preset_values() {
        let presenter = PresetsPresenter::new(Arc::new(PresetCatalog::new(Default::default())));
        let snapshot = (&presenter as &dyn PresetsApi).snapshot();
        assert!(snapshot.records.is_empty());
        assert!(
            snapshot
                .providers
                .iter()
                .any(|item| item.id == "gemini" && item.supported)
        );
        assert!(
            snapshot
                .providers
                .iter()
                .filter(|item| item.supported)
                .count()
                == 1
        );
        let draft = presenter.new_draft();
        assert!(draft.name.is_empty());
        let PresetDefinition::Gemini(config) = draft.definition;
        assert!(config.model.is_empty());
        assert_eq!(config.endpoint, GeminiTemplate::GOOGLE_ENDPOINT);
    }
}
