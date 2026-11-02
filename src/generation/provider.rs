use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use thiserror::Error;

use super::{GenerationPreset, ProviderKind, ProviderTemplate, ResponseEffect, StructuredResponse};
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

#[derive(Debug, Clone)]
pub struct ProposedResponse {
    pub text: Arc<str>,
    pub structured: Arc<StructuredResponse>,
    pub effects: Arc<[ResponseEffect]>,
}

impl ProposedResponse {
    pub fn from_structured(structured: StructuredResponse) -> Result<Self, GenerationError> {
        structured.validate()?;
        structured.ensure_supported_effects()?;
        Ok(Self {
            text: structured.full_text(),
            effects: structured.proposed_effects()?.into(),
            structured: Arc::new(structured),
        })
    }
}

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StructuredResponse, GenerationError>> + Send + 'a>>;

pub trait GenerationProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn generate(&self, context: CharacterContext, template: ProviderTemplate)
    -> ProviderFuture<'_>;
}

pub struct ResponseGenerator {
    providers: HashMap<ProviderKind, Arc<dyn GenerationProvider>>,
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
        })
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
        let structured = provider.generate(request.context, template).await?;
        ProposedResponse::from_structured(structured)
    }
}
