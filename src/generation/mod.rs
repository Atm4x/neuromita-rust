mod conversation;
pub(crate) use conversation::LlmConversation;
mod credentials;
mod effects;
pub mod gemini;
mod presets;
pub use credentials::{ApplicationPresetCredentials, CredentialReference};
mod provider;
pub mod python_import;
mod structured;
mod transforms;
pub use effects::ResponseEffect;
pub use presets::{
    EnvironmentPresetCredentials, GeminiConnectionEdit, GeminiPresetDefinition,
    JsonPresetRepository, PresetCatalog, PresetCatalogError, PresetCatalogSnapshot,
    PresetConnectionEdit, PresetCredentials, PresetDefinition, PresetId, PresetRecord,
    PresetRepository, PresetTemplate,
};
pub use structured::{
    RESPONSE_PROTOCOL_VERSION, ResponseSegment, SegmentIntent, StructuredResponse, ToolCall,
    WorkingState,
};
pub use transforms::{MessageTransform, apply_message_transforms};

pub use provider::{
    GenerationError, GenerationInput, GenerationObserver, GenerationProvider, GenerationRequest,
    PreparedRequest, PromptMessage, PromptRole, ProposedResponse, ProviderFuture, ProviderReply,
    ProviderUsage, ResponseGenerator,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Gemini,
}

#[derive(Debug, Clone)]
pub enum ProviderTemplate {
    Gemini(gemini::GeminiTemplate),
}

impl ProviderTemplate {
    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Gemini(_) => ProviderKind::Gemini,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenerationPreset {
    template: ProviderTemplate,
}

impl GenerationPreset {
    pub fn new(template: ProviderTemplate) -> Self {
        Self { template }
    }
    pub fn template(&self) -> &ProviderTemplate {
        &self.template
    }
    pub(crate) fn diagnostic_redaction_values(&self) -> Vec<String> {
        match &self.template {
            ProviderTemplate::Gemini(template) => template.diagnostic_redaction_values(),
        }
    }
}
