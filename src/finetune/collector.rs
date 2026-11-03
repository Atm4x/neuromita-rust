use std::{path::Path, sync::Arc};

use serde_json::{Value, json};

use super::*;
use crate::{
    generation::{
        GenerationError, GenerationObserver, PreparedRequest, PromptRole, ProviderKind,
        ProviderReply, ProviderUsage,
    },
    request::{RequestId, RequestSource, TurnIntent},
    timeline::EventId,
};

#[derive(Debug, Clone)]
pub struct CaptureScope {
    pub request_id: RequestId,
    pub character_id: String,
    pub intent: TurnIntent,
    pub source: RequestSource,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureStatus {
    pub sample_id: Option<SampleId>,
    pub warning: Option<Arc<str>>,
}

/// Synchronous provider observer. A successful HTTP body is captured here
/// before provider decoding and application validation; persistence happens
/// afterwards and never replaces the provider result.
pub struct FineTuneCaptureObserver {
    enabled: bool,
    scope: CaptureScope,
    prepared: Option<PreparedRequest>,
    captured: Option<CapturedProviderResponse>,
    status: CaptureStatus,
}

impl FineTuneCaptureObserver {
    pub fn status(&self) -> &CaptureStatus {
        &self.status
    }

    pub fn captured(&self) -> Option<CapturedProviderResponse> {
        self.captured.clone()
    }

    fn record_warning(&mut self, warning: impl Into<Arc<str>>) {
        self.status.warning = Some(warning.into());
    }
}

impl GenerationObserver for FineTuneCaptureObserver {
    fn request_prepared(&mut self, request: PreparedRequest) {
        self.prepared = Some(request);
    }

    fn response_received(&mut self, response: ProviderReply) {
        if !self.enabled
            || self.captured.is_some()
            || !response.complete
            || !response
                .status
                .is_some_and(|status| (200..300).contains(&status))
        {
            return;
        }
        let Some(request) = self.prepared.take() else {
            self.record_warning("dataset capture omitted: provider request metadata unavailable");
            return;
        };
        self.captured = Some(CapturedProviderResponse {
            scope: self.scope.clone(),
            request,
            response,
        });
    }

    fn failed(&mut self, _error: &GenerationError) {}
}

#[derive(Clone)]
pub struct FineTuneCollector {
    store: FineTuneStore,
    enabled: bool,
    retention: Retention,
}

impl FineTuneCollector {
    pub fn from_settings(settings: &FineTuneSettings) -> Result<Self, FineTuneError> {
        settings.validate()?;
        Ok(Self {
            store: FineTuneStore::open(settings.data_dir.clone())?,
            enabled: settings.enabled,
            retention: settings.retention,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn store(&self) -> &FineTuneStore {
        &self.store
    }

    pub fn data_dir(&self) -> &Path {
        self.store.root()
    }

    pub fn observer(&self, scope: CaptureScope) -> FineTuneCaptureObserver {
        FineTuneCaptureObserver {
            enabled: self.enabled,
            scope,
            prepared: None,
            captured: None,
            status: CaptureStatus::default(),
        }
    }

    /// Persists one immutable provider observation outside the provider
    /// callback. The handler awaits this before acceptance, so a successful
    /// response is durable before any message or character effect is applied.
    pub async fn persist(
        &self,
        capture: CapturedProviderResponse,
    ) -> Result<CaptureStatus, FineTuneError> {
        if !self.enabled {
            return Ok(CaptureStatus::default());
        }
        let sample = build_sample(&capture.scope, &capture.request, &capture.response);
        let sample_id = sample.id;
        let store = self.store.clone();
        let retention = self.retention;
        let outcome = tokio::task::spawn_blocking(move || store.append_sample(&sample, retention))
            .await
            .map_err(|error| {
                FineTuneError::SettingsInvalid(format!("dataset capture task failed: {error}"))
            })??;
        let mut status = CaptureStatus {
            sample_id: Some(sample_id),
            warning: None,
        };
        if let Err(error) = outcome.retention {
            status.warning = Some(error.to_string().into());
        }
        Ok(status)
    }

    pub async fn record_outcome(
        &self,
        sample_id: SampleId,
        request_id: RequestId,
        accepted_event_id: Option<EventId>,
        outcome: SampleOutcome,
    ) -> Result<(), FineTuneError> {
        let record = OutcomeRecord {
            sample_id,
            request_id: Some(request_id),
            accepted_event_id,
            outcome,
            recorded_at: UtcTimestamp::now(),
        };
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.append_outcome(&record))
            .await
            .map_err(|error| {
                FineTuneError::SettingsInvalid(format!("dataset outcome task failed: {error}"))
            })??;
        Ok(())
    }

    pub fn rate(&self, sample_id: SampleId, rating: Rating) -> Result<RatingUpdate, FineTuneError> {
        self.store.set_rating(sample_id, rating)
    }
}

#[derive(Debug, Clone)]
pub struct CapturedProviderResponse {
    pub scope: CaptureScope,
    pub request: PreparedRequest,
    pub response: ProviderReply,
}

fn build_sample(
    scope: &CaptureScope,
    request: &PreparedRequest,
    response: &ProviderReply,
) -> FineTuneSample {
    let response_text = extract_provider_text(&response.raw_body);
    let messages = request
        .prompt
        .iter()
        .map(|message| {
            SampleMessage::text(
                match message.role {
                    PromptRole::System => MessageRole::System,
                    PromptRole::User => MessageRole::User,
                    PromptRole::Assistant => MessageRole::Assistant,
                },
                message.text.as_ref(),
            )
        })
        .collect();
    let model = Some(request.model.to_string());
    let mut sample = FineTuneSample::new(
        UtcTimestamp::now(),
        scope.character_id.clone(),
        model,
        messages,
        response_text.clone(),
    );
    sample.id = SampleId::for_request(scope.request_id);
    sample.character_name = scope.character_id.clone();
    let (provider_name, protocol_id, dialect_id) = match request.provider {
        ProviderKind::Gemini => (
            Some("gemini".into()),
            Some("google_gemini_default".into()),
            Some("generate_content".into()),
        ),
    };
    sample.provider_name = provider_name;
    sample.protocol_id = protocol_id;
    sample.dialect_id = dialect_id;
    sample.temperature = Some(f64::from(request.temperature));
    sample.max_tokens = Some(u64::from(request.max_output_tokens));
    sample.request_payload = Some(request.payload_json.to_string());
    sample.provider_response = Some(response.raw_body.to_string());
    sample.raw_response = Some(response_text);
    sample.request_id = Some(scope.request_id);
    sample.intent = Some(match scope.intent {
        TurnIntent::Reply(_) => "reply".into(),
        TurnIntent::React(_) => "react".into(),
        TurnIntent::Initiate => "initiate".into(),
    });
    sample.source = Some(match scope.source {
        RequestSource::Presenter => "presenter".into(),
        RequestSource::Client(id) => format!("client:{id}"),
    });
    sample.usage = response.usage.map(usage_payload);
    sample.usage_source = response
        .usage
        .is_some()
        .then_some("provider_response_usage_metadata".into());
    sample
}

fn usage_payload(usage: ProviderUsage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens,
    })
}

/// Gemini's provider answer text is extracted without structured decoding so
/// malformed application JSON still leaves the exact provider payload intact.
fn extract_provider_text(raw_body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw_body) else {
        return String::new();
    };
    value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect()
}
