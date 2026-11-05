use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::*;
use crate::{
    character::{AcceptanceContext, CharacterError, CharacterHandle},
    finetune::{
        CaptureScope, CaptureStatus, FineTuneCaptureObserver, FineTuneCollector, SampleOutcome,
    },
    games::GamePolicy,
    generation::{
        GenerationError, GenerationObserver, GenerationRequest, LlmConversation, PreparedRequest,
        PresetCatalog, ProviderReply, ResponseGenerator,
    },
    knowledge::KnowledgeRetrieval,
    settings::SettingsOwner,
    timeline::selection::TimelineSelection,
    voice::{VoiceError, VoiceOptions, VoiceProvider},
};

#[derive(Clone)]
pub(crate) struct MessageHandler {
    pub(crate) character: CharacterHandle,
    pub(crate) registry: Arc<RequestRegistry>,
    pub(crate) generator: Arc<ResponseGenerator>,
    pub(crate) knowledge: Arc<KnowledgeRetrieval>,
    pub(crate) voice: Arc<dyn VoiceProvider>,
    pub(crate) settings: Arc<SettingsOwner>,
    pub(crate) presets: Arc<PresetCatalog>,
    pub(crate) prompt_preparation: super::prompt_preparation::PromptPreparation,
    pub(crate) finetune: Option<FineTuneCollector>,
    pub(crate) selection_updates: watch::Receiver<TimelineSelection>,
    pub(crate) context_updates: watch::Receiver<u64>,
    pub(crate) games: Arc<GamePolicy>,
}

impl MessageHandler {
    pub(crate) async fn run_queue(
        self,
        mut queue: mpsc::Receiver<RequestId>,
        stop: CancellationToken,
    ) {
        loop {
            let id = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                item = queue.recv() => match item { Some(id) => id, None => break },
            };
            let Some(record) = self.registry.get(id) else {
                continue;
            };
            let Some(message) = record.try_start() else {
                continue;
            };
            let handler = self.clone();
            let stopping = stop.clone();
            let span = tracing::info_span!("generation", request_id = %id,
                character = %record.snapshot().metadata.character_id);
            let task = tokio::spawn(
                async move {
                    let _guard = WorkGuard(Arc::clone(&record));
                    tracing::info!("request started");
                    handler
                        .process(Arc::clone(&record), message, stopping)
                        .await;
                    let snapshot = record.snapshot();
                    let outcome = match &snapshot.state {
                        RequestState::Cancelled => "cancelled",
                        RequestState::Finished(RequestOutcome::Success(_)) => "success",
                        RequestState::Finished(RequestOutcome::TextGenerationFailed(_)) => {
                            "text_error"
                        }
                        RequestState::Finished(RequestOutcome::VoiceGenerationFailed {
                            ..
                        }) => "voice_error",
                        RequestState::Finished(RequestOutcome::InternalError(_)) => {
                            "internal_error"
                        }
                        RequestState::Pending | RequestState::Processing(_) => "incomplete",
                    };
                    tracing::info!(outcome, "request finished");
                }
                .instrument(span),
            );
            if let Err(error) = task.await {
                tracing::error!(request_id = %id, panicked = error.is_panic(), "request task terminated");
            }
        }
        queue.close();
        while let Some(id) = queue.recv().await {
            if let Some(record) = self.registry.get(id) {
                record.cancel_with_reason(CancelReason::QueueDrain);
                record.finish_cancelled();
            }
        }
    }

    async fn process(
        &self,
        record: Arc<RequestRecord>,
        message: Box<dyn GenerationCommand>,
        stop: CancellationToken,
    ) {
        let id = record.snapshot().metadata.id;
        let settings = match record
            .admitted_settings()
            .map(Ok)
            .unwrap_or_else(|| self.settings.snapshot_all())
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                record.finish(RequestOutcome::InternalError(
                    record.failure(error.to_string()),
                ));
                return;
            }
        };
        let voice_settings = settings.values.voice.clone();
        let voice_enabled = match message.source() {
            RequestSource::Presenter | RequestSource::Scheduler => voice_settings.in_launcher,
            RequestSource::Client(_) => voice_settings.in_launcher,
            RequestSource::Game(_) => voice_settings.in_game,
        };
        let prepared = tokio::select! {
            biased;
            _ = record.cancellation.cancelled() => { record.finish_cancelled(); return; }
            _ = stop.cancelled() => {
                record.cancel_with_reason(CancelReason::Shutdown);
                record.finish_cancelled();
                return;
            }
            result = self.prompt_preparation.prepare(
                &self.character,
                id,
                Arc::from(message.guidance()),
                message.intent(),
                message.at(),
                record.admission(),
                record.admitted_prompt(),
            ) => result,
        };
        let (context, prompt, post_dsl) = match prepared {
            Ok(prepared) => (prepared.context, prepared.prompt, prepared.post_dsl),
            Err(super::prompt_preparation::PromptPreparationError::Character(
                CharacterError::StaleState,
            )) => {
                record.cancel_with_reason(CancelReason::ContextChanged);
                record.finish_cancelled();
                return;
            }
            Err(error) => {
                record.finish(RequestOutcome::InternalError(
                    record.failure(error.to_string()),
                ));
                return;
            }
        };
        let voice_options = voice_enabled.then_some(VoiceOptions {
            voice: voice_settings.voice,
            volume: voice_settings.volume,
        });
        let acceptance = AcceptanceContext {
            anchor: context.anchor,
            intent: context.focus.intent,
            state_revision: context.state.revision,
            turn: context.turn,
            game: context.game_key,
            game_guard: context.game_guard,
        };
        record.prepared(context.clone());
        let preset_id = settings
            .values
            .selected_preset(message.character_id(), message.intent());
        let preset = match record
            .admitted_preset()
            .map(Some)
            .map(Ok)
            .unwrap_or_else(|| self.presets.get(preset_id))
        {
            Ok(Some(preset)) => preset,
            Ok(None) => {
                record.finish(RequestOutcome::InternalError(
                    record.failure(format!("assigned preset {:?} does not exist", preset_id)),
                ));
                return;
            }
            Err(error) => {
                record.finish(RequestOutcome::InternalError(
                    record.failure(error.to_string()),
                ));
                return;
            }
        };
        record.set_redaction_values(preset.diagnostic_redaction_values());
        record.set_preset_description(format!("{:?}", preset));
        let mut observer = self
            .finetune
            .as_ref()
            .filter(|collector| collector.enabled())
            .map(|collector| {
                collector.observer(CaptureScope {
                    request_id: id,
                    character_id: message.character_id().to_string(),
                    intent: message.intent(),
                    source: message.source(),
                })
            });
        let generated = tokio::select! {
            biased;
            _ = record.cancellation.cancelled() => GenerationRun::Cancelled,
            _ = stop.cancelled() => {
                record.cancel_with_reason(CancelReason::Shutdown);
                GenerationRun::Cancelled
            }
            _ = selection_changed(self.selection_updates.clone(), acceptance.turn.selection) => {
                record.cancel_with_reason(CancelReason::SelectionChanged);
                GenerationRun::Cancelled
            }
            _ = context_epoch_changed(self.context_updates.clone(), acceptance.turn.context_epoch) => {
                record.cancel_with_reason(CancelReason::ContextChanged);
                GenerationRun::Cancelled
            }
            response = async {
                let query = LlmConversation::memory_query(&context);
                let selected = self.knowledge
                    .select_with_relations(context.character_id, &context.memories, &context.memory_islands, &context.memory_relations, &query)
                    .await
                    .map_err(|error| GenerationError::Response(
                        format!("knowledge retrieval failed: {error}")
                    ))?;
                let request = GenerationRequest::new(context, preset, selected)
                    .with_prompt(prompt)
                    .with_post_dsl(post_dsl);
                let mut combined = RecordObserver {
                    record: &record,
                    capture: observer.as_mut(),
                };
                self.generator.generate_observed(request, &mut combined).await
            } => GenerationRun::Response(response),
        };
        let capture = self.persist_observation(observer.as_ref());
        if capture.sample_id.is_some() || capture.warning.is_some() {
            record.dataset_capture(capture.sample_id, capture.warning.clone());
        }
        let proposed = match generated {
            GenerationRun::Response(Ok(proposed)) => proposed,
            GenerationRun::Response(Err(error)) => {
                self.finish_dataset_outcome(
                    &record,
                    &capture,
                    None,
                    SampleOutcome::AcceptanceFailed,
                );
                record.finish(RequestOutcome::TextGenerationFailed(error));
                return;
            }
            GenerationRun::Cancelled => {
                self.finish_dataset_outcome(
                    &record,
                    &capture,
                    None,
                    SampleOutcome::CancelledBeforeAccept,
                );
                record.finish_cancelled();
                return;
            }
        };
        let text = Arc::clone(proposed.text());
        if !record.text_ready(Arc::clone(&text)) {
            self.finish_dataset_outcome(
                &record,
                &capture,
                None,
                SampleOutcome::CancelledBeforeAccept,
            );
            record.finish_cancelled();
            return;
        }
        let voice_attempted = voice_options.is_some();
        let voice_result = match voice_options {
            None => Ok(None),
            Some(options) => {
                if !record.generating_voice() {
                    self.finish_dataset_outcome(
                        &record,
                        &capture,
                        None,
                        SampleOutcome::CancelledBeforeAccept,
                    );
                    record.finish_cancelled();
                    return;
                }
                let voice = Arc::clone(&self.voice);
                let mut voice_task =
                    tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                        voice.synthesize(text, options).await
                    }));
                let result = tokio::select! {
                    biased;
                    _ = record.cancellation.cancelled() => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        self.finish_dataset_outcome(
                            &record,
                            &capture,
                            None,
                            SampleOutcome::CancelledBeforeAccept,
                        );
                        record.finish_cancelled();
                        return;
                    }
                    _ = stop.cancelled() => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        self.finish_dataset_outcome(
                            &record,
                            &capture,
                            None,
                            SampleOutcome::CancelledBeforeAccept,
                        );
                        record.cancel_with_reason(CancelReason::Shutdown);
                        record.finish_cancelled();
                        return;
                    }
                    _ = selection_changed(self.selection_updates.clone(), acceptance.turn.selection) => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        self.finish_dataset_outcome(
                            &record,
                            &capture,
                            None,
                            SampleOutcome::CancelledBeforeAccept,
                        );
                        record.cancel_with_reason(CancelReason::SelectionChanged);
                        record.finish_cancelled();
                        return;
                    }
                    _ = context_epoch_changed(self.context_updates.clone(), acceptance.turn.context_epoch) => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        self.finish_dataset_outcome(
                            &record,
                            &capture,
                            None,
                            SampleOutcome::CancelledBeforeAccept,
                        );
                        record.cancel_with_reason(CancelReason::ContextChanged);
                        record.finish_cancelled();
                        return;
                    }
                    result = &mut voice_task => result,
                };
                match result {
                    Ok(Ok(audio)) => Ok(Some(audio)),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(VoiceError::Generation(format!(
                        "voice worker stopped: {error}"
                    ))),
                }
            }
        };
        if voice_attempted {
            record.voice_finished();
        }
        if !record.begin_accepting() {
            self.finish_dataset_outcome(
                &record,
                &capture,
                None,
                SampleOutcome::CancelledBeforeAccept,
            );
            record.finish_cancelled();
            return;
        }
        let (audio, voice_error) = match voice_result {
            Ok(audio) => (audio, None),
            Err(error) => (None, Some(error)),
        };
        let accepted = match self
            .character
            .accept(id, acceptance, proposed, audio, message.audience().clone())
            .await
        {
            Ok(response) => {
                let event_id = response.event_id;
                self.finish_dataset_outcome(
                    &record,
                    &capture,
                    Some(event_id),
                    if voice_error.is_some() {
                        SampleOutcome::AcceptedVoiceFailed
                    } else {
                        SampleOutcome::Accepted
                    },
                );
                response
            }
            Err(error) => {
                self.finish_dataset_outcome(
                    &record,
                    &capture,
                    None,
                    SampleOutcome::AcceptanceFailed,
                );
                record.finish(RequestOutcome::InternalError(
                    record.failure(error.to_string()),
                ));
                return;
            }
        };
        record.mark_applied();
        if let Some(invite) = accepted.game_invite.clone()
            && (!invite.auto_gated || self.games.auto_reactions)
        {
            let request = crate::request::ReactRequest::new(
                message.character_id(),
                invite.event_id,
                invite.instruction,
                RequestSource::Presenter,
            );
            if let Err(error) = crate::api::ChatApi::react(request) {
                tracing::warn!(%error, "game follow-up reaction was not admitted");
            }
        }
        let outcome = match voice_error {
            Some(error) => RequestOutcome::VoiceGenerationFailed {
                response: accepted,
                error,
            },
            None => RequestOutcome::Success(accepted),
        };
        record.finish(outcome);
    }

    fn finish_dataset_outcome(
        &self,
        record: &Arc<RequestRecord>,
        capture: &CaptureStatus,
        accepted_event_id: Option<crate::timeline::EventId>,
        outcome: SampleOutcome,
    ) {
        let (Some(collector), Some(sample_id)) = (&self.finetune, capture.sample_id) else {
            return;
        };
        if let Err(error) = collector.enqueue_outcome(
            sample_id,
            record.snapshot().metadata.id,
            accepted_event_id,
            outcome,
        ) {
            let warning = match capture.warning.as_deref() {
                Some(warning) => format!("{warning}; dataset outcome: {error}"),
                None => format!("dataset outcome: {error}"),
            };
            record.dataset_capture(Some(sample_id), Some(Arc::from(warning)));
        }
    }

    fn persist_observation(&self, observer: Option<&FineTuneCaptureObserver>) -> CaptureStatus {
        let (Some(collector), Some(observer)) = (&self.finetune, observer) else {
            return CaptureStatus::default();
        };
        let Some(captured) = observer.captured() else {
            return observer.status().clone();
        };
        collector
            .enqueue(captured)
            .unwrap_or_else(|error| CaptureStatus {
                sample_id: None,
                warning: Some(Arc::from(error.to_string())),
            })
    }
}

enum GenerationRun {
    Response(Result<crate::generation::ProposedResponse, GenerationError>),
    Cancelled,
}

async fn context_epoch_changed(mut updates: watch::Receiver<u64>, captured: u64) {
    loop {
        if *updates.borrow_and_update() != captured {
            return;
        }
        if updates.changed().await.is_err() {
            return;
        }
    }
}

async fn selection_changed(
    mut updates: watch::Receiver<TimelineSelection>,
    captured: TimelineSelection,
) {
    loop {
        if *updates.borrow_and_update() != captured {
            return;
        }
        if updates.changed().await.is_err() {
            return;
        }
    }
}

struct WorkGuard(Arc<RequestRecord>);

struct RecordObserver<'a> {
    record: &'a RequestRecord,
    capture: Option<&'a mut FineTuneCaptureObserver>,
}

impl GenerationObserver for RecordObserver<'_> {
    fn request_prepared(&mut self, request: PreparedRequest) {
        if let Some(capture) = &mut self.capture {
            capture.request_prepared(request.clone());
        }
        self.record.provider_requested(request);
    }

    fn response_received(&mut self, response: ProviderReply) {
        if let Some(capture) = &mut self.capture {
            capture.response_received(response.clone());
        }
        self.record.provider_replied(response);
    }

    fn failed(&mut self, error: &GenerationError) {
        if let Some(capture) = &mut self.capture {
            capture.failed(error);
        }
        self.record.provider_failed(error);
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        if !self.0.snapshot().state.is_terminal() {
            self.0.finish(RequestOutcome::InternalError(
                self.0.failure("request worker stopped unexpectedly".into()),
            ));
        }
    }
}
