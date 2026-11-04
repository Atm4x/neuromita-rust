mod options;
mod template;
pub use options::{GeminiRequestOptions, GeminiThinking, SystemInstructionMode, ThinkingLevel};
mod wire;

pub use template::{GeminiTemplate, GenerationSettings, PresetError};

use std::time::{Instant, SystemTime};

use super::provider::GenerationInput;
use super::{
    GenerationError, GenerationObserver, GenerationProvider, PreparedRequest, ProviderFuture,
    ProviderKind, ProviderReply, ProviderTemplate,
};

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
        observer: &mut dyn GenerationObserver,
    ) -> Result<super::StructuredResponse, GenerationError> {
        let encoded =
            wire::encode_observed(context.context, &template, &context.prepared_knowledge);
        let payload_json = serde_json::to_string(&encoded.body).map_err(|error| {
            GenerationError::Response(format!("cannot encode provider request: {error}"))
        })?;
        let mut endpoint = template.request_url();
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        observer.request_prepared(PreparedRequest {
            provider: ProviderKind::Gemini,
            model: template.model().into(),
            endpoint: endpoint.as_str().into(),
            temperature: template
                .options()
                .send_temperature
                .then(|| template.settings().temperature()),
            max_output_tokens: template
                .options()
                .send_max_output_tokens
                .then(|| template.settings().max_output_tokens()),
            prompt: encoded.prompt.into(),
            payload_json: payload_json.as_str().into(),
            captured_at: SystemTime::now(),
        });
        let started = Instant::now();
        let mut response = match self
            .client
            .post(template.request_url())
            .timeout(template.settings().timeout())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload_json)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let error = GenerationError::Transport(error.without_url().to_string());
                observer.failed(&error);
                return Err(error);
            }
        };
        let status = response.status();
        let mut bytes = Vec::new();
        loop {
            let chunk = match response.chunk().await {
                Ok(chunk) => chunk,
                Err(error) => {
                    let error = GenerationError::Transport(error.without_url().to_string());
                    observer.response_received(ProviderReply {
                        status: Some(status.as_u16()),
                        raw_body: String::from_utf8_lossy(&bytes).into_owned().as_str().into(),
                        answer_text: None,
                        complete: false,
                        finish_reason: None,
                        usage: None,
                        elapsed: started.elapsed(),
                    });
                    observer.failed(&error);
                    return Err(error);
                }
            };
            let Some(chunk) = chunk else {
                break;
            };
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                let remaining = (8usize * 1024 * 1024).saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
                observer.response_received(ProviderReply {
                    status: Some(status.as_u16()),
                    raw_body: String::from_utf8_lossy(&bytes).into_owned().as_str().into(),
                    answer_text: None,
                    complete: false,
                    finish_reason: None,
                    usage: None,
                    elapsed: started.elapsed(),
                });
                let error = GenerationError::Response("response exceeds 8 MiB".into());
                observer.failed(&error);
                return Err(error);
            }
            bytes.extend_from_slice(&chunk);
        }
        let raw_body = String::from_utf8_lossy(&bytes).into_owned();
        let observation = wire::observation_metadata(&bytes);
        observer.response_received(ProviderReply {
            status: Some(status.as_u16()),
            raw_body: raw_body.as_str().into(),
            answer_text: observation.answer_text,
            complete: true,
            finish_reason: observation.finish_reason,
            usage: observation.usage,
            elapsed: started.elapsed(),
        });
        if !status.is_success() {
            let error = GenerationError::Http(status.as_u16());
            observer.failed(&error);
            return Err(error);
        }
        match wire::decode(&bytes) {
            Ok(response) => Ok(response),
            Err(error) => {
                observer.failed(&error);
                Err(error)
            }
        }
    }
}

impl GenerationProvider for GeminiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, context: GenerationInput, template: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            match template {
                ProviderTemplate::Gemini(template) => {
                    self.execute(context, template, &mut NoopObserver).await
                }
            }
        })
    }

    fn generate_observed<'a>(
        &'a self,
        context: GenerationInput,
        template: ProviderTemplate,
        observer: &'a mut dyn GenerationObserver,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            match template {
                ProviderTemplate::Gemini(template) => {
                    self.execute(context, template, observer).await
                }
            }
        })
    }
}

struct NoopObserver;
impl GenerationObserver for NoopObserver {}
