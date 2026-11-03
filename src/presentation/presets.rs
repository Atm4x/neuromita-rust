use crate::localization::{LocalizedText, i18n};
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
    pub pricing: &'static str,
    pub badge_kind: &'static str,
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetParameterOption {
    pub id: &'static str,
    pub wire_key: &'static str,
    pub title: LocalizedText,
    pub description: LocalizedText,
    pub editable: bool,
    pub sent: bool,
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
    pub parameters: Vec<PresetParameterOption>,
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
            parameters: Self::parameters(),
        }
    }

    pub fn providers() -> Vec<ProviderOption> {
        const CATALOG: &[(&str, &str, &str, &str, &str, &str, bool)] = &[
            (
                "gemini",
                "Google AI Studio",
                "google",
                "https://ai.google.dev/gemini-api/docs",
                "mixed",
                "",
                true,
            ),
            (
                "openrouter",
                "OpenRouter",
                "openrouter",
                "https://openrouter.ai/docs/quickstart",
                "mixed",
                "",
                false,
            ),
            (
                "mistral",
                "Mistral AI",
                "mistral",
                "https://docs.mistral.ai/",
                "mixed",
                "",
                false,
            ),
            ("aiio", "Ai.iO", "aiio", "", "paid", "", false),
            ("proxyapi", "ProxyAPI", "proxyapi", "", "paid", "ru", false),
            (
                "groq",
                "Groq",
                "groq",
                "https://console.groq.com/docs/overview",
                "mixed",
                "",
                false,
            ),
            (
                "together",
                "Together AI",
                "together",
                "https://docs.together.ai/",
                "mixed",
                "",
                false,
            ),
            ("chutes", "Chutes", "chutes", "", "paid", "", false),
            (
                "lmstudio",
                "LM Studio",
                "lmstudio",
                "https://lmstudio.ai/docs",
                "free",
                "local",
                false,
            ),
            (
                "ollama",
                "Ollama",
                "ollama",
                "https://docs.ollama.com/",
                "free",
                "local",
                false,
            ),
            (
                "kodikrouter",
                "KodikRouter",
                "kodikrouter",
                "",
                "paid",
                "ru",
                false,
            ),
            (
                "openai",
                "OpenAI Responses (API key)",
                "openai",
                "https://platform.openai.com/docs/api-reference/responses",
                "paid",
                "",
                false,
            ),
            (
                "codex",
                "ChatGPT Plan (Codex)",
                "openai",
                "",
                "paid",
                "subscription",
                false,
            ),
        ];
        CATALOG
            .iter()
            .map(
                |&(id, name, icon, documentation, pricing, badge_kind, supported)| ProviderOption {
                    id,
                    name,
                    icon,
                    schema_label: if id == "gemini" {
                        "Google Gemini API"
                    } else {
                        ""
                    },
                    documentation,
                    pricing,
                    badge_kind,
                    supported,
                },
            )
            .collect()
    }

    pub fn parameters() -> Vec<PresetParameterOption> {
        vec![
            PresetParameterOption {
                id: "temperature",
                wire_key: "temperature",
                title: i18n("settings.parameter.temperature.title", "Температура"),
                description: i18n(
                    "settings.parameter.temperature.description",
                    "Разнообразие ответов: ниже — предсказуемее, выше — свободнее.",
                ),
                editable: true,
                sent: true,
            },
            PresetParameterOption {
                id: "max_tokens",
                wire_key: "maxOutputTokens",
                title: i18n("settings.parameter.max_tokens.title", "Макс. токенов"),
                description: i18n(
                    "settings.parameter.max_tokens.description",
                    "Лимит выходных токенов, включая токены размышления.",
                ),
                editable: true,
                sent: true,
            },
            PresetParameterOption {
                id: "top_p",
                wire_key: "topP",
                title: i18n("settings.parameter.top_p.title", "Порог Top-P"),
                description: i18n(
                    "settings.parameter.top_p.description",
                    "Ограничивает выбор слов по суммарной вероятности.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "presence_penalty",
                wire_key: "presencePenalty",
                title: i18n(
                    "settings.parameter.presence_penalty.title",
                    "Штраф присутствия",
                ),
                description: i18n(
                    "settings.parameter.presence_penalty.description",
                    "Снижает вероятность повторного использования уже встречавшихся слов.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "frequency_penalty",
                wire_key: "frequencyPenalty",
                title: i18n(
                    "settings.parameter.frequency_penalty.title",
                    "Штраф частоты",
                ),
                description: i18n(
                    "settings.parameter.frequency_penalty.description",
                    "Снижает вероятность слов пропорционально числу их повторений.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "top_k",
                wire_key: "topK",
                title: i18n("settings.parameter.top_k.title", "Лимит Top-K"),
                description: i18n(
                    "settings.parameter.top_k.description",
                    "Число кандидатов при выборе следующего токена.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "thinking_level",
                wire_key: "thinkingLevel",
                title: i18n(
                    "settings.parameter.thinking_level.title",
                    "Глубина размышлений",
                ),
                description: i18n(
                    "settings.parameter.thinking_level.description",
                    "Низкий thinkingLevel сокращает размышления, но не отключает мышление.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "thinking_budget",
                wire_key: "thinkingBudget",
                title: i18n(
                    "settings.parameter.thinking_budget.title",
                    "Бюджет мышления Gemini",
                ),
                description: i18n(
                    "settings.parameter.thinking_budget.description",
                    "Лимит токенов размышления: -1 — автоматически, 0 — отключить, если модель поддерживает.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "include_thoughts",
                wire_key: "includeThoughts",
                title: i18n(
                    "settings.parameter.include_thoughts.title",
                    "Возвращать сводки размышлений",
                ),
                description: i18n(
                    "settings.parameter.include_thoughts.description",
                    "Возвращает сводки размышлений и не управляет объёмом мышления.",
                ),
                editable: false,
                sent: false,
            },
            PresetParameterOption {
                id: "timeout",
                wire_key: "",
                title: i18n("settings.parameter.timeout.title", "Тайм-аут"),
                description: i18n(
                    "settings.parameter.timeout.description",
                    "Время ожидания ответа API в миллисекундах.",
                ),
                editable: true,
                sent: false,
            },
        ]
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
    fn presenter_snapshot_preserves_catalog_values() {
        let presenter = PresetsPresenter::new(Arc::new(PresetCatalog::new(Default::default())));
        let mut draft = presenter.new_draft();
        draft.name = "test-preset".into();
        let PresetDefinition::Gemini(definition) = &mut draft.definition;
        definition.model = "test-model".into();
        definition.credential_env = "TEST_KEY_ENV".into();
        definition.temperature = 1.3;
        definition.max_output_tokens = 2500;
        let created = (&presenter as &dyn PresetsApi).create(draft).unwrap();
        let snapshot = (&presenter as &dyn PresetsApi).snapshot();
        let record = snapshot
            .records
            .iter()
            .find(|record| record.id == created)
            .unwrap();
        assert_eq!(record.name, "test-preset");
        assert_eq!(record.model, "test-model");
        assert_eq!(record.credential_env, "TEST_KEY_ENV");
        assert_eq!(record.temperature, 1.3);
        assert_eq!(record.max_output_tokens, 2500);
        assert!(
            snapshot
                .parameters
                .iter()
                .any(|option| option.id == "temperature" && option.editable)
        );
        assert!(
            snapshot
                .parameters
                .iter()
                .any(|option| option.id == "top_p" && !option.editable)
        );
        (&presenter as &dyn PresetsApi).require(created).unwrap();
        (&presenter as &dyn PresetsApi).delete(created).unwrap();
        assert!(
            (&presenter as &dyn PresetsApi)
                .snapshot()
                .records
                .is_empty()
        );
    }

    #[test]
    fn provider_pricing_and_badges_match_python_templates() {
        let providers = PresetsPresenter::providers();
        for (id, pricing, kind) in [
            ("gemini", "mixed", ""),
            ("openrouter", "mixed", ""),
            ("mistral", "mixed", ""),
            ("aiio", "paid", ""),
            ("proxyapi", "paid", "ru"),
            ("groq", "mixed", ""),
            ("together", "mixed", ""),
            ("chutes", "paid", ""),
            ("lmstudio", "free", "local"),
            ("ollama", "free", "local"),
            ("kodikrouter", "paid", "ru"),
            ("openai", "paid", ""),
            ("codex", "paid", "subscription"),
        ] {
            let option = providers.iter().find(|option| option.id == id).unwrap();
            assert_eq!((option.pricing, option.badge_kind), (pricing, kind));
        }
    }

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
