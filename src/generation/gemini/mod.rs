mod template;
mod wire;

pub use template::{GeminiTemplate, GenerationSettings, PresetError};

use super::provider::GenerationInput;
use super::{GenerationError, GenerationProvider, ProviderFuture, ProviderKind, ProviderTemplate};

pub struct GeminiProvider {
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new() -> Result<Self, GenerationError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| GenerationError::Transport(error.without_url().to_string()))?;
        Ok(Self { client })
    }

    async fn execute(
        &self,
        context: GenerationInput,
        template: GeminiTemplate,
    ) -> Result<super::StructuredResponse, GenerationError> {
        let body = wire::encode_with_memory(context.context, &template, &context.memory_handoff);
        let mut response = self
            .client
            .post(template.request_url())
            .timeout(template.settings().timeout())
            .json(&body)
            .send()
            .await
            .map_err(|error| GenerationError::Transport(error.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(GenerationError::Http(response.status().as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| GenerationError::Transport(error.without_url().to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(GenerationError::Response("response exceeds 8 MiB".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        wire::decode(&bytes)
    }
}

impl GenerationProvider for GeminiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, context: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            match template {
                ProviderTemplate::Gemini(template) => self.execute(context, template).await,
            }
        })
    }
}
