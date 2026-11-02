mod conversation;
mod effects;
pub mod embedding;
pub mod gemini;
pub mod gemini_embedding;
mod memory_context;
mod presets;
mod provider;
pub mod semantic_memory;
pub use memory_context::lexical_selected_ids;
mod structured;
pub use effects::ResponseEffect;
pub use presets::{
    EnvironmentPresetCredentials, GeminiPresetDefinition, JsonPresetRepository, PresetCatalog,
    PresetCatalogError, PresetCatalogSnapshot, PresetCredentials, PresetDefinition, PresetId,
    PresetRecord, PresetRepository,
};
pub use structured::{
    RESPONSE_PROTOCOL_VERSION, ResponseSegment, SegmentIntent, StructuredResponse, ToolCall,
    WorkingState,
};

pub use provider::{
    GenerationError, GenerationInput, GenerationProvider, GenerationRequest, ProposedResponse,
    ProviderFuture, ResponseGenerator,
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
}
