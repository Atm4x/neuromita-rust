use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use thiserror::Error;

use super::{
    GenerationPreset, ProviderKind, ProviderTemplate, StructuredResponse,
    conversation::LlmConversation,
    memory_context::MemoryHandoff,
    retrieval_fusion::{FusionConfig, FusionMode, fuse},
    semantic_memory::{SemanticMemoryRetriever, SemanticRetrievalFilter},
};
use crate::character::CharacterContext;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum GenerationError {
    #[error("provider is not registered: {0:?}")]
    Unavailable(ProviderKind),
    #[error("provider is registered twice: {0:?}")]
    Duplicate(ProviderKind),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("provider returned HTTP {0}")]
    Http(u16),
    #[error("invalid provider response: {0}")]
    Response(String),
}

pub struct GenerationRequest {
    pub context: CharacterContext,
    pub preset: GenerationPreset,
}

impl GenerationRequest {
    pub fn new(context: CharacterContext, preset: GenerationPreset) -> Self {
        Self { context, preset }
    }
}

pub use crate::turn::TurnProposal as ProposedResponse;

/// A generated response sees an immutable character snapshot and an explicit memory handoff.
/// Lexical and semantic retrieval have distinct, explicit prompt contracts.
#[derive(Clone)]
pub struct GenerationInput {
    pub context: CharacterContext,
    pub memory_handoff: MemoryHandoff,
}

impl std::ops::Deref for GenerationInput {
    type Target = CharacterContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl ProposedResponse {
    pub fn from_structured(structured: StructuredResponse) -> Result<Self, GenerationError> {
        structured.validate()?;
        structured.ensure_supported_effects()?;
        let text = structured.full_text();
        let effects = structured.proposed_effects()?;
        Ok(Self::validated(text, structured.segments, effects))
    }
}

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StructuredResponse, GenerationError>> + Send + 'a>>;

pub trait GenerationProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn generate(&self, context: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_>;
}

pub struct ResponseGenerator {
    providers: HashMap<ProviderKind, Arc<dyn GenerationProvider>>,
    semantic_memory: Option<Arc<SemanticMemoryRetriever>>,
    memory_fusion_mode: FusionMode,
}

impl ResponseGenerator {
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn GenerationProvider>>,
    ) -> Result<Self, GenerationError> {
        let mut registered = HashMap::new();
        for provider in providers {
            let kind = provider.kind();
            if registered.insert(kind, provider).is_some() {
                return Err(GenerationError::Duplicate(kind));
            }
        }
        Ok(Self {
            providers: registered,
            semantic_memory: None,
            memory_fusion_mode: FusionMode::VectorOnly,
        })
    }

    pub fn with_semantic_memory(mut self, retriever: Arc<SemanticMemoryRetriever>) -> Self {
        self.semantic_memory = Some(retriever);
        self
    }

    /// Explicitly opt into fusion; the default remains exact vector-only retrieval.
    /// The operation does not install or load any embedding or reranking model.
    pub fn with_semantic_memory_fusion(
        mut self,
        retriever: Arc<SemanticMemoryRetriever>,
        mode: FusionMode,
    ) -> Result<Self, GenerationError> {
        fuse(
            crate::identity::CharacterId::Mita,
            &[],
            FusionConfig {
                mode,
                limit: 1024,
                minimum_graph: 0,
            },
        )
        .map_err(|error| GenerationError::Response(error.to_string()))?;
        self.semantic_memory = Some(retriever);
        self.memory_fusion_mode = mode;
        Ok(self)
    }

    pub async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<ProposedResponse, GenerationError> {
        let template = request.preset.template().clone();
        let kind = template.kind();
        let provider = self
            .providers
            .get(&kind)
            .ok_or(GenerationError::Unavailable(kind))?;
        let memory_handoff = if let Some(retriever) = &self.semantic_memory {
            let query = LlmConversation::memory_query(&request.context);
            let result = retriever
                .retrieve_with_fusion(
                    request.context.character_id,
                    &request.context.memories,
                    &query,
                    SemanticRetrievalFilter::all(),
                    self.memory_fusion_mode,
                )
                .await
                .map_err(|error| {
                    GenerationError::Response(format!("semantic memory retrieval failed: {error}"))
                })?;
            tracing::debug!(
                character = %request.context.character_id,
                selected = result.selected_ids.len(),
                embedding_requests = result.cost.embedding_requests,
                embedded_texts = result.cost.embedded_texts,
                embedded_characters = result.cost.embedded_characters,
                cache_hits = result.cost.cache_hits,
                "semantic memory retrieval finished"
            );
            MemoryHandoff::Semantic(result.rendered)
        } else {
            MemoryHandoff::Lexical
        };
        let input = GenerationInput {
            context: request.context,
            memory_handoff,
        };
        let structured = provider.generate(input, template).await?;
        ProposedResponse::from_structured(structured)
    }
}
