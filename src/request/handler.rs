use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::*;
use crate::{
    character::{AcceptanceContext, CharacterError, CharacterHandle},
    generation::{
        GenerationError, GenerationRequest, LlmConversation, PresetCatalog, ResponseGenerator,
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
    pub(crate) selection_updates: watch::Receiver<TimelineSelection>,
    pub(crate) context_updates: watch::Receiver<u64>,
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
                record.cancel();
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
        let prepared = tokio::select! {
            biased;
            _ = record.cancellation.cancelled() => { record.finish_cancelled(); return; }
            context = self.character.prepare(
                id,
                Arc::from(message.guidance()),
                message.intent(),
                message.at(),
                record.admission(),
            ) => context,
        };
        let context = match prepared {
            Ok(context) => context,
            Err(CharacterError::StaleState) => {
                record.cancel();
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
        let settings = match self.settings.snapshot_all() {
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
            RequestSource::Presenter => voice_settings.in_launcher,
            RequestSource::Client(_) => voice_settings.in_game,
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
        };
        record.prepared(context.clone());
        let preset_id = settings
            .values
            .selected_preset(message.character_id(), message.intent());
        let preset = match self.presets.get(preset_id) {
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
        record.set_preset_description(format!("{:?}", preset));
        let generated = tokio::select! {
            biased;
            _ = record.cancellation.cancelled() => { record.finish_cancelled(); return; }
            _ = selection_changed(self.selection_updates.clone(), acceptance.turn.selection) => {
                record.cancel();
                record.finish_cancelled();
                return;
            }
            _ = context_epoch_changed(self.context_updates.clone(), acceptance.turn.context_epoch) => {
                record.cancel();
                record.finish_cancelled();
                return;
            }
            response = async {
                let query = LlmConversation::memory_query(&context);
                let selected = self.knowledge
                    .select(context.character_id, &context.memories, &query)
                    .await
                    .map_err(|error| GenerationError::Response(
                        format!("knowledge retrieval failed: {error}")
                    ))?;
                self.generator
                    .generate(GenerationRequest::new(context, preset, selected))
                    .await
            } => response,
        };
        let proposed = match generated {
            Ok(proposed) => proposed,
            Err(error) => {
                record.finish(RequestOutcome::TextGenerationFailed(error));
                return;
            }
        };
        let text = Arc::clone(proposed.text());
        record.text_ready(Arc::clone(&text));
        let voice_result = match voice_options {
            None => Ok(None),
            Some(options) => {
                if !record.generating_voice(Arc::clone(&text)) {
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
                        record.finish_cancelled();
                        return;
                    }
                    _ = stop.cancelled() => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        record.cancel();
                        record.finish_cancelled();
                        return;
                    }
                    _ = selection_changed(self.selection_updates.clone(), acceptance.turn.selection) => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        record.cancel();
                        record.finish_cancelled();
                        return;
                    }
                    _ = context_epoch_changed(self.context_updates.clone(), acceptance.turn.context_epoch) => {
                        voice_task.abort();
                        let _ = voice_task.await;
                        record.cancel();
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
        if !record.begin_accepting() {
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
            Ok(response) => response,
            Err(error) => {
                record.finish(RequestOutcome::InternalError(
                    record.failure(error.to_string()),
                ));
                return;
            }
        };
        record.mark_applied();
        let outcome = match voice_error {
            Some(error) => RequestOutcome::VoiceGenerationFailed {
                response: accepted,
                error,
            },
            None => RequestOutcome::Success(accepted),
        };
        record.finish(outcome);
    }
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

impl Drop for WorkGuard {
    fn drop(&mut self) {
        if !self.0.snapshot().state.is_terminal() {
            self.0.finish(RequestOutcome::InternalError(
                self.0.failure("request worker stopped unexpectedly".into()),
            ));
        }
    }
}
