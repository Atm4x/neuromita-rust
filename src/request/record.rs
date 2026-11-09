use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{registry::RegistryInner, *};
use crate::identity::EventId;
#[cfg(test)]
use crate::timeline::selection::TimelineSelection;
use crate::timeline::{RegenerateRecoveryOutcome, selection::TimelineAdmission};

pub(crate) struct RequestRecord {
    inner: Mutex<Execution>,
    updates: watch::Sender<Arc<RequestSnapshot>>,
    pub(crate) cancellation: CancellationToken,
    registry: Weak<RegistryInner>,
    admission: TimelineAdmission,
    settings_snapshot: Option<Arc<crate::settings::SettingsSnapshot>>,
    admitted_preset: Option<crate::generation::GenerationPreset>,
    admitted_prompt: Option<Arc<crate::prompts::PromptSetSnapshot>>,
    cancel_ingress: Option<Arc<dyn super::RegenerateCancellation>>,
    regenerate_protocol: Option<RegenerateProtocol>,
}

struct Execution {
    snapshot: Arc<RequestSnapshot>,
    dataset: DatasetCapture,
    phase: EffectPhase,
    cancel_reason: Option<CancelReason>,
    message: Option<Box<dyn GenerationCommand>>,
    trace: Option<RequestTrace>,
    draft_text: Option<Arc<str>>,
    pending_terminal: Option<PendingTerminal>,
}

#[derive(Clone)]
enum PendingTerminal {
    Failure(RequestOutcome),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegenerateTerminalReconciliation {
    Abandoned,
    AcceptedElsewhere(EventId),
}

#[derive(Clone)]
pub struct RequestHandle {
    pub(crate) record: Arc<RequestRecord>,
}

impl std::fmt::Debug for RequestHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestHandle")
            .field("id", &self.id())
            .finish()
    }
}

impl RequestHandle {
    pub fn id(&self) -> RequestId {
        self.record.snapshot().metadata.id
    }
    pub fn snapshot(&self) -> Arc<RequestSnapshot> {
        self.record.snapshot()
    }
    pub fn cancel(&self) -> CancelResult {
        self.record.cancel()
    }
    pub(crate) fn cancel_with_reason(&self, reason: CancelReason) -> CancelResult {
        self.record.cancel_with_reason(reason)
    }
    pub fn observe(&self) -> RequestObserver {
        RequestObserver {
            updates: self.record.updates.subscribe(),
            initial: true,
            finished: false,
        }
    }
    pub async fn wait(&self) -> Result<Arc<RequestSnapshot>, ObserveError> {
        let mut observer = self.observe();
        while let Some(snapshot) = observer.next().await? {
            if snapshot.is_resolved() {
                return Ok(snapshot);
            }
        }
        Err(ObserveError::ClosedBeforeCompletion)
    }
}

pub struct RequestObserver {
    updates: watch::Receiver<Arc<RequestSnapshot>>,
    initial: bool,
    finished: bool,
}

impl RequestObserver {
    pub async fn next(&mut self) -> Result<Option<Arc<RequestSnapshot>>, ObserveError> {
        if self.finished {
            return Ok(None);
        }
        if !self.initial {
            self.updates
                .changed()
                .await
                .map_err(|_| ObserveError::ClosedBeforeCompletion)?;
        }
        self.initial = false;
        let snapshot = Arc::clone(&self.updates.borrow_and_update());
        self.finished = snapshot.is_resolved();
        if let Some(DurableRequestResolution::AcceptedElsewhere(event)) =
            snapshot.durable_resolution
        {
            return Err(ObserveError::DurablyAcceptedElsewhere(event));
        }
        Ok(Some(snapshot))
    }
}

impl RequestRecord {
    pub(crate) fn new(
        message: Box<dyn GenerationCommand>,
        id: RequestId,
        admission: TimelineAdmission,
        settings_snapshot: Option<Arc<crate::settings::SettingsSnapshot>>,
        admitted_preset: Option<crate::generation::GenerationPreset>,
        admitted_prompt: Option<Arc<crate::prompts::PromptSetSnapshot>>,
        registry: Weak<RegistryInner>,
        cancel_ingress: Option<Arc<dyn super::RegenerateCancellation>>,
    ) -> Arc<Self> {
        Self::new_with_protocol(
            message,
            id,
            admission,
            settings_snapshot,
            admitted_preset,
            admitted_prompt,
            registry,
            cancel_ingress,
            None,
        )
    }

    pub(crate) fn new_with_protocol(
        message: Box<dyn GenerationCommand>,
        id: RequestId,
        admission: TimelineAdmission,
        settings_snapshot: Option<Arc<crate::settings::SettingsSnapshot>>,
        admitted_preset: Option<crate::generation::GenerationPreset>,
        admitted_prompt: Option<Arc<crate::prompts::PromptSetSnapshot>>,
        registry: Weak<RegistryInner>,
        cancel_ingress: Option<Arc<dyn super::RegenerateCancellation>>,
        regenerate_protocol: Option<RegenerateProtocol>,
    ) -> Arc<Self> {
        let snapshot = Arc::new(RequestSnapshot {
            metadata: RequestMetadata {
                id,
                character_id: message.character_id(),
                source: message.source(),
                intent: message.intent(),
            },
            revision: 0,
            state: RequestState::Pending,
            streamed_json: None,
            streamed_text: None,
            tool_activity: None,
            dataset: DatasetCapture::default(),
            cancel_reason: None,
            durable_resolution: None,
        });
        let (updates, _) = watch::channel(Arc::clone(&snapshot));
        let diagnostics_limits = registry
            .upgrade()
            .map_or_else(DiagnosticsLimits::default, |registry| {
                registry.diagnostics_limits()
            });
        let trace = RequestTrace::new(
            snapshot.metadata.clone(),
            message.guidance(),
            diagnostics_limits,
        );
        Arc::new(Self {
            inner: Mutex::new(Execution {
                snapshot,
                dataset: DatasetCapture::default(),
                phase: EffectPhase::Working,
                cancel_reason: None,
                message: Some(message),
                trace: Some(trace),
                draft_text: None,
                pending_terminal: None,
            }),
            updates,
            cancellation: CancellationToken::new(),
            registry,
            admission,
            settings_snapshot,
            admitted_preset,
            admitted_prompt,
            cancel_ingress,
            regenerate_protocol,
        })
    }

    pub(crate) fn handle(self: &Arc<Self>) -> RequestHandle {
        RequestHandle {
            record: Arc::clone(self),
        }
    }
    pub(crate) fn snapshot(&self) -> Arc<RequestSnapshot> {
        Arc::clone(&self.inner.lock().expect("request mutex poisoned").snapshot)
    }

    fn publish(&self, inner: &mut Execution, state: RequestState) {
        let snapshot = Arc::new(RequestSnapshot {
            metadata: inner.snapshot.metadata.clone(),
            revision: inner.snapshot.revision + 1,
            state,
            streamed_json: None,
            streamed_text: None,
            tool_activity: None,
            dataset: inner.dataset.clone(),
            cancel_reason: inner.cancel_reason,
            durable_resolution: inner.snapshot.durable_resolution,
        });
        inner.snapshot = Arc::clone(&snapshot);
        let phase = inner.phase;
        if let Some(trace) = &mut inner.trace {
            trace.push_transition(snapshot.revision, phase, &snapshot.state);
        }
        self.updates.send_replace(snapshot);
    }

    fn retire(&self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.complete(self);
        }
    }

    pub(crate) fn set_preset_description(&self, description: String) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.set_preset_description(description);
        }
    }

    pub(crate) fn set_redaction_values(&self, values: Vec<String>) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.configure_redaction(values);
        }
    }

    pub(crate) fn admitted_settings(&self) -> Option<Arc<crate::settings::SettingsSnapshot>> {
        self.settings_snapshot.clone()
    }
    pub(crate) fn admitted_preset(&self) -> Option<crate::generation::GenerationPreset> {
        self.admitted_preset.clone()
    }

    pub(crate) fn admitted_prompt(&self) -> Option<Arc<crate::prompts::PromptSetSnapshot>> {
        self.admitted_prompt.clone()
    }

    pub(crate) fn admission(&self) -> TimelineAdmission {
        self.admission
    }

    pub(crate) fn regenerate_protocol(&self) -> Option<RegenerateProtocol> {
        self.regenerate_protocol
    }

    fn abandon_durable(
        &self,
        ingress: &dyn super::RegenerateCancellation,
        request: RequestId,
    ) -> Result<(), crate::timeline::domain::TimelineError> {
        match self
            .regenerate_protocol
            .unwrap_or(RegenerateProtocol::LegacyFork)
        {
            RegenerateProtocol::LegacyFork => ingress.abandon_regenerate(request, self.admission),
            RegenerateProtocol::SameBranch => {
                ingress.abandon_same_branch_regenerate(request, self.admission)
            }
        }
    }

    fn inspect_durable(
        &self,
        ingress: &dyn super::RegenerateCancellation,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, crate::timeline::domain::TimelineError> {
        match self
            .regenerate_protocol
            .unwrap_or(RegenerateProtocol::LegacyFork)
        {
            RegenerateProtocol::LegacyFork => ingress
                .inspect_regenerate(self.admission.selection.branch_id, request)
                .map(|v| v.outcome),
            RegenerateProtocol::SameBranch => {
                ingress.inspect_same_branch_regenerate(self.admission.selection.branch_id, request)
            }
        }
    }

    pub(crate) fn try_start(&self) -> Option<Box<dyn GenerationCommand>> {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.is_resolved() || !matches!(inner.snapshot.state, RequestState::Pending) {
            return None;
        }
        let message = inner.message.take()?;
        if let Some(trace) = &mut inner.trace {
            trace.mark_started();
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::GeneratingText),
        );
        Some(message)
    }

    pub(crate) fn cancel(&self) -> CancelResult {
        self.cancel_with_reason(CancelReason::User)
    }

    pub(crate) fn cancel_with_reason(&self, reason: CancelReason) -> CancelResult {
        if let Some(ingress) = &self.cancel_ingress {
            {
                let inner = self.inner.lock().expect("request mutex poisoned");
                if inner.snapshot.is_resolved() {
                    return CancelResult::AlreadyFinished;
                }
                if inner.phase == EffectPhase::Applied {
                    return CancelResult::TooLate;
                }
            }
            let request = self.snapshot().metadata.id;
            if let Err(error) = self.abandon_durable(ingress.as_ref(), request) {
                if matches!(
                    error,
                    crate::timeline::domain::TimelineError::Storage(_)
                        | crate::timeline::domain::TimelineError::ConcurrentWrite
                ) {
                    let mut inner = self.inner.lock().expect("request mutex poisoned");
                    if !inner.snapshot.is_resolved() && inner.phase != EffectPhase::Applied {
                        inner.pending_terminal = Some(PendingTerminal::Cancelled);
                    }
                }
                if matches!(error, crate::timeline::domain::TimelineError::Conflict) {
                    self.observe_external_acceptance(ingress.as_ref(), request);
                }
                tracing::warn!(%request, %error, "durable regenerate abandonment refused");
                return CancelResult::TooLate;
            }
        }
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.is_resolved() {
            return CancelResult::AlreadyFinished;
        }
        if self.cancel_ingress.is_none()
            && matches!(inner.phase, EffectPhase::Accepting | EffectPhase::Applied)
        {
            return CancelResult::TooLate;
        }
        if inner.phase == EffectPhase::Applied {
            return CancelResult::TooLate;
        }
        inner.cancel_reason.get_or_insert(reason);
        inner.pending_terminal = None;
        self.cancellation.cancel();
        if matches!(inner.snapshot.state, RequestState::Pending) {
            inner.phase = EffectPhase::Discarded;
            inner.message.take();
            self.publish(&mut inner, RequestState::Cancelled);
            drop(inner);
            self.retire();
            CancelResult::Cancelled
        } else {
            CancelResult::Requested
        }
    }

    pub(crate) fn begin_accepting(&self) -> bool {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.phase != EffectPhase::Working
            || inner.snapshot.is_resolved()
            || inner.pending_terminal.is_some()
            || self.cancellation.is_cancelled()
        {
            return false;
        }
        let Some(text) = inner.draft_text.clone() else {
            return false;
        };
        inner.phase = EffectPhase::Accepting;
        if let Some(trace) = &mut inner.trace {
            trace.accepting();
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::Accepting { text }),
        );
        true
    }

    pub(crate) fn mark_applied(&self) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        assert_eq!(inner.phase, EffectPhase::Accepting);
        inner.phase = EffectPhase::Applied;
        let snapshot = Arc::clone(&inner.snapshot);
        if let Some(trace) = &mut inner.trace {
            trace.push_transition(snapshot.revision, EffectPhase::Applied, &snapshot.state);
        }
    }

    fn observe_external_acceptance(
        &self,
        ingress: &dyn super::RegenerateCancellation,
        request: RequestId,
    ) -> Option<EventId> {
        let RegenerateRecoveryOutcome::AcceptedReply(event) =
            self.inspect_durable(ingress, request).ok()?
        else {
            return None;
        };
        self.seal_external_acceptance(event);
        Some(event)
    }

    fn seal_external_acceptance(&self, event: EventId) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.is_resolved() || inner.phase == EffectPhase::Applied {
            return;
        }
        inner.pending_terminal = None;
        inner.phase = EffectPhase::Applied;
        inner.message.take();
        let snapshot = Arc::new(RequestSnapshot {
            metadata: inner.snapshot.metadata.clone(),
            revision: inner.snapshot.revision + 1,
            state: inner.snapshot.state.clone(),
            streamed_json: None,
            streamed_text: None,
            tool_activity: None,
            cancel_reason: inner.cancel_reason,
            dataset: inner.dataset.clone(),
            durable_resolution: Some(DurableRequestResolution::AcceptedElsewhere(event)),
        });
        inner.snapshot = Arc::clone(&snapshot);
        if let Some(trace) = &mut inner.trace {
            trace.accepted_elsewhere(event, snapshot.revision);
            trace.finished();
        }
        self.updates.send_replace(snapshot);
        drop(inner);
        self.cancellation.cancel();
        self.retire();
    }

    #[cfg(test)]
    pub(crate) fn externally_accepted(&self) -> Option<EventId> {
        let inner = self.inner.lock().expect("request mutex poisoned");
        match inner.snapshot.durable_resolution {
            Some(DurableRequestResolution::AcceptedElsewhere(event)) => Some(event),
            _ => None,
        }
    }

    pub(crate) fn reconcile_pending_terminal(
        &self,
    ) -> Result<Option<RegenerateTerminalReconciliation>, String> {
        let pending = {
            let inner = self.inner.lock().expect("request mutex poisoned");
            inner.pending_terminal.clone()
        };
        let Some(pending) = pending else {
            return Ok(None);
        };
        let Some(ingress) = &self.cancel_ingress else {
            return Ok(None);
        };
        let request = self.snapshot().metadata.id;
        match self.abandon_durable(ingress.as_ref(), request) {
            Ok(()) => {
                match pending {
                    PendingTerminal::Failure(outcome) => self.finish_volatile(outcome),
                    PendingTerminal::Cancelled => self.finish_cancelled_volatile(),
                }
                Ok(Some(RegenerateTerminalReconciliation::Abandoned))
            }
            Err(error) => {
                if let Some(event) = self.observe_external_acceptance(ingress.as_ref(), request) {
                    return Ok(Some(RegenerateTerminalReconciliation::AcceptedElsewhere(
                        event,
                    )));
                }
                Err(error.to_string())
            }
        }
    }

    pub(crate) fn finish(&self, outcome: RequestOutcome) {
        if self.snapshot().is_resolved() {
            return;
        }
        if self
            .inner
            .lock()
            .expect("request mutex poisoned")
            .pending_terminal
            .is_some()
        {
            if let Err(error) = self.reconcile_pending_terminal() {
                tracing::warn!(%error, "regenerate terminal still requires reconciliation");
            }
            return;
        }
        if matches!(
            outcome,
            RequestOutcome::TextGenerationFailed(_) | RequestOutcome::InternalError(_)
        ) && let Some(ingress) = &self.cancel_ingress
        {
            let request = self.snapshot().metadata.id;
            let should_abandon = {
                let inner = self.inner.lock().expect("request mutex poisoned");
                !inner.snapshot.is_resolved() && inner.phase != EffectPhase::Applied
            };
            if !should_abandon {
                if self
                    .observe_external_acceptance(ingress.as_ref(), request)
                    .is_some()
                {
                    return;
                }
            } else if let Err(error) = self.abandon_durable(ingress.as_ref(), request) {
                if self
                    .observe_external_acceptance(ingress.as_ref(), request)
                    .is_some()
                {
                    return;
                }
                let mut inner = self.inner.lock().expect("request mutex poisoned");
                if !inner.snapshot.is_resolved() && inner.phase != EffectPhase::Applied {
                    inner.pending_terminal = Some(PendingTerminal::Failure(outcome));
                }
                tracing::warn!(%request, %error, "regenerate failure remains pending durable reconciliation");
                return;
            }
        }
        self.finish_volatile(outcome);
    }

    fn finish_volatile(&self, outcome: RequestOutcome) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.is_resolved() {
            return;
        }
        inner.pending_terminal = None;
        let cancelled = inner.phase != EffectPhase::Applied && self.cancellation.is_cancelled();
        if inner.phase != EffectPhase::Applied {
            inner.phase = if matches!(
                outcome,
                RequestOutcome::Success(_) | RequestOutcome::VoiceGenerationFailed { .. }
            ) {
                EffectPhase::Applied
            } else {
                EffectPhase::Discarded
            };
        }
        let state = if cancelled {
            RequestState::Cancelled
        } else {
            RequestState::Finished(outcome)
        };
        if let Some(trace) = &mut inner.trace {
            match &state {
                RequestState::Finished(outcome) => trace.outcome(outcome),
                RequestState::Cancelled => trace.cancelled(),
                _ => {}
            }
            trace.finished();
        }
        self.publish(&mut inner, state);
        drop(inner);
        self.retire();
    }

    pub(crate) fn finish_cancelled(&self) {
        {
            let inner = self.inner.lock().expect("request mutex poisoned");
            if inner.snapshot.is_resolved()
                || inner.phase == EffectPhase::Applied
                || (inner.phase == EffectPhase::Accepting && !self.cancellation.is_cancelled())
            {
                return;
            }
        }
        if let Some(ingress) = &self.cancel_ingress {
            let request = self.snapshot().metadata.id;
            if let Err(error) = self.abandon_durable(ingress.as_ref(), request) {
                if self
                    .observe_external_acceptance(ingress.as_ref(), request)
                    .is_some()
                {
                    return;
                }
                let mut inner = self.inner.lock().expect("request mutex poisoned");
                if !inner.snapshot.is_resolved() && inner.phase != EffectPhase::Applied {
                    inner.pending_terminal = Some(PendingTerminal::Cancelled);
                }
                tracing::warn!(%request, %error, "regenerate cancellation remains pending durable reconciliation");
                return;
            }
        }
        self.finish_cancelled_volatile();
    }

    fn finish_cancelled_volatile(&self) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.snapshot.is_resolved()
            || inner.phase == EffectPhase::Applied
            || (inner.phase == EffectPhase::Accepting && !self.cancellation.is_cancelled())
        {
            return;
        }
        inner.pending_terminal = None;
        inner.phase = EffectPhase::Discarded;
        inner.message.take();
        self.cancellation.cancel();
        if let Some(trace) = &mut inner.trace {
            trace.cancelled();
            trace.finished();
        }
        self.publish(&mut inner, RequestState::Cancelled);
        drop(inner);
        self.retire();
    }

    pub(crate) fn prepared(&self, context: crate::character::CharacterContext) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.prepared(&context);
        }
    }
    pub(crate) fn dataset_capture(
        &self,
        sample_id: Option<crate::finetune::SampleId>,
        warning: Option<Arc<str>>,
    ) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        inner.dataset.sample_id = sample_id;
        inner.dataset.warning = warning;
        let snapshot = Arc::new(RequestSnapshot {
            metadata: inner.snapshot.metadata.clone(),
            revision: inner.snapshot.revision + 1,
            state: inner.snapshot.state.clone(),
            streamed_json: inner.snapshot.streamed_json.clone(),
            streamed_text: inner.snapshot.streamed_text.clone(),
            tool_activity: inner.snapshot.tool_activity.clone(),
            cancel_reason: inner.cancel_reason,
            dataset: inner.dataset.clone(),
            durable_resolution: inner.snapshot.durable_resolution,
        });
        inner.snapshot = Arc::clone(&snapshot);
        self.updates.send_replace(snapshot);
    }
    pub(crate) fn text_ready(&self, text: Arc<str>) -> bool {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.phase != EffectPhase::Working
            || self.cancellation.is_cancelled()
            || !matches!(
                inner.snapshot.state,
                RequestState::Processing(ProcessingStage::GeneratingText)
            )
        {
            return false;
        }
        inner.draft_text = Some(Arc::clone(&text));
        if let Some(trace) = &mut inner.trace {
            trace.text_ready(&text);
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::TextReady { text }),
        );
        true
    }
    pub(crate) fn failure(&self, message: String) -> InternalFailure {
        InternalFailure {
            message: Arc::from(message),
            partial_text: self
                .inner
                .lock()
                .expect("request mutex poisoned")
                .draft_text
                .clone(),
        }
    }
    pub(crate) fn take_trace(&self) -> Option<Arc<RequestTrace>> {
        self.inner
            .lock()
            .expect("request mutex poisoned")
            .trace
            .take()
            .map(Arc::new)
    }
    pub(crate) fn trace(&self) -> Option<Arc<RequestTrace>> {
        self.inner
            .lock()
            .expect("request mutex poisoned")
            .trace
            .clone()
            .map(Arc::new)
    }
    pub(crate) fn generating_voice(&self) -> bool {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if inner.phase != EffectPhase::Working
            || self.cancellation.is_cancelled()
            || !matches!(
                inner.snapshot.state,
                RequestState::Processing(ProcessingStage::TextReady { .. })
            )
        {
            return false;
        }
        let Some(text) = inner.draft_text.clone() else {
            return false;
        };
        if let Some(trace) = &mut inner.trace {
            trace.voice_started();
        }
        self.publish(
            &mut inner,
            RequestState::Processing(ProcessingStage::GeneratingVoice { text }),
        );
        true
    }

    pub(crate) fn voice_finished(&self) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.voice_finished();
        }
    }

    pub(crate) fn provider_requested(&self, request: crate::generation::PreparedRequest) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.provider_requested(request);
        }
    }

    pub(crate) fn stream_delta(&self, delta: &str) {
        const LIMIT: usize = 64 * 1024;
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if self.cancellation.is_cancelled()
            || !matches!(
                inner.snapshot.state,
                RequestState::Processing(ProcessingStage::GeneratingText)
            )
        {
            return;
        }
        let previous = inner.snapshot.streamed_json.as_deref().unwrap_or("");
        if delta.is_empty() || previous.len().saturating_add(delta.len()) > LIMIT {
            return;
        }
        let mut combined = String::with_capacity(previous.len() + delta.len());
        combined.push_str(previous);
        combined.push_str(delta);
        let mut snapshot = (*inner.snapshot).clone();
        snapshot.revision += 1;
        if snapshot
            .tool_activity
            .as_ref()
            .is_some_and(|activity| !activity.executing)
        {
            snapshot.tool_activity = None;
        }
        snapshot.streamed_text = super::stream_preview::spoken_prefix(&combined).map(Arc::from);
        snapshot.streamed_json = Some(Arc::from(combined));
        inner.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&inner.snapshot));
    }

    pub(crate) fn tool_started(
        &self,
        name: &str,
        args: &serde_json::Map<String, serde_json::Value>,
        spoken: &str,
    ) {
        const MAX_ARGS_BYTES: usize = 2048;
        const MAX_SPOKEN_BYTES: usize = 4096;
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if self.cancellation.is_cancelled()
            || !matches!(
                inner.snapshot.state,
                RequestState::Processing(ProcessingStage::GeneratingText)
            )
        {
            return;
        }
        let mut display_args = serde_json::to_string(args).unwrap_or_default();
        if display_args.len() > MAX_ARGS_BYTES {
            let mut boundary = MAX_ARGS_BYTES.saturating_sub(3);
            while !display_args.is_char_boundary(boundary) {
                boundary -= 1;
            }
            display_args.truncate(boundary);
            display_args.push_str("...");
        }
        let mut snapshot = (*inner.snapshot).clone();
        snapshot.revision += 1;
        snapshot.streamed_json = None;
        let mut visible = spoken.trim();
        if visible.len() > MAX_SPOKEN_BYTES {
            let mut boundary = MAX_SPOKEN_BYTES;
            while !visible.is_char_boundary(boundary) {
                boundary -= 1;
            }
            visible = &visible[..boundary];
        }
        snapshot.streamed_text = (!visible.is_empty()).then(|| Arc::from(visible));
        let full_json = serde_json::json!({"tool_call": {"name": name, "args": args}}).to_string();
        let display_json = if full_json.len() > 4096 {
            serde_json::json!({"tool_call": {"name": name, "args": "[too large to display]"}})
                .to_string()
        } else {
            full_json
        };
        snapshot.tool_activity = Some(ToolActivity {
            name: Arc::from(name),
            args: Arc::from(display_args),
            json: Arc::from(display_json),
            executing: true,
        });
        inner.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&inner.snapshot));
    }

    pub(crate) fn tool_finished(&self) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if self.cancellation.is_cancelled() || inner.snapshot.is_resolved() {
            return;
        }
        let mut snapshot = (*inner.snapshot).clone();
        let Some(activity) = snapshot.tool_activity.as_mut() else {
            return;
        };
        if !activity.executing {
            return;
        }
        activity.executing = false;
        snapshot.revision += 1;
        inner.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&inner.snapshot));
    }

    pub(crate) fn stream_reset(&self) {
        let mut inner = self.inner.lock().expect("request mutex poisoned");
        if self.cancellation.is_cancelled() || inner.snapshot.is_resolved() {
            return;
        }
        let mut snapshot = (*inner.snapshot).clone();
        snapshot.revision += 1;
        snapshot.streamed_json = None;
        if snapshot.tool_activity.is_none() {
            snapshot.streamed_text = None;
        }
        inner.snapshot = Arc::new(snapshot);
        self.updates.send_replace(Arc::clone(&inner.snapshot));
    }

    pub(crate) fn provider_replied(&self, reply: crate::generation::ProviderReply) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.provider_replied(reply);
        }
    }

    pub(crate) fn provider_failed(&self, error: &crate::generation::GenerationError) {
        if let Some(trace) = &mut self.inner.lock().expect("request mutex poisoned").trace {
            trace.provider_failed(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;
    use crate::{character::CharacterId, identity::EventId};

    fn request() -> ReplyRequest {
        ReplyRequest::new(CharacterId::Crazy, EventId::new(), RequestSource::Presenter)
    }

    fn selection() -> TimelineSelection {
        TimelineSelection {
            branch_id: crate::timeline::domain::BranchId::new(),
            generation: 1,
        }
    }

    #[test]
    fn cancellation_and_acceptance_have_one_winner() {
        for _ in 0..64 {
            let registry = RequestRegistry::new(4);
            let handle = registry.register(
                Box::new(request()),
                RequestId::new(),
                TimelineAdmission {
                    selection: selection(),
                    context_epoch: 0,
                },
            );
            let record = Arc::clone(&handle.record);
            assert!(record.try_start().is_some());
            assert!(record.text_ready(Arc::from("ready")));
            let barrier = Barrier::new(2);
            let (accepted, cancelled) = std::thread::scope(|scope| {
                let accepting = scope.spawn(|| {
                    barrier.wait();
                    record.begin_accepting()
                });
                let cancelling = scope.spawn(|| {
                    barrier.wait();
                    handle.cancel()
                });
                (accepting.join().unwrap(), cancelling.join().unwrap())
            });
            if accepted {
                assert_eq!(cancelled, CancelResult::TooLate);
                assert!(!record.cancellation.is_cancelled());
                record.finish(RequestOutcome::InternalError(
                    Arc::<str>::from("simulated acceptance rejection").into(),
                ));
                assert!(matches!(handle.snapshot().state, RequestState::Finished(_)));
            } else {
                assert_eq!(cancelled, CancelResult::Requested);
                record.finish_cancelled();
                assert!(matches!(handle.snapshot().state, RequestState::Cancelled));
            }
            assert_eq!(registry.active_count(), 0);
        }
    }

    #[test]
    fn voice_remains_cancellable_until_acceptance() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        let record = Arc::clone(&handle.record);
        assert!(record.try_start().is_some());
        assert_eq!(handle.snapshot().provisional_text(), None);
        assert!(handle.snapshot().can_cancel());
        assert!(record.text_ready(Arc::from("provisional text")));
        assert_eq!(
            handle.snapshot().provisional_text(),
            Some("provisional text")
        );
        assert!(matches!(
            handle.snapshot().state,
            RequestState::Processing(ProcessingStage::TextReady { .. })
        ));
        assert!(record.generating_voice());
        assert_eq!(
            handle.snapshot().provisional_text(),
            Some("provisional text")
        );
        assert!(handle.snapshot().can_cancel());
        let trace = record.trace().unwrap();
        assert_eq!(
            trace.transitions.last().unwrap().phase,
            EffectPhase::Working
        );
        assert_eq!(handle.cancel(), CancelResult::Requested);
        assert!(!record.begin_accepting());
        record.finish_cancelled();
        assert!(matches!(handle.snapshot().state, RequestState::Cancelled));
        assert_eq!(handle.snapshot().provisional_text(), None);
        assert!(!handle.snapshot().can_cancel());
    }

    #[test]
    fn accepting_reuses_provisional_text_and_closes_cancellation_boundary() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        let record = Arc::clone(&handle.record);
        assert!(!record.begin_accepting());
        assert!(record.try_start().is_some());
        assert!(!record.begin_accepting());
        assert!(record.text_ready(Arc::from("accepted after voice")));
        assert!(record.generating_voice());
        let voice_revision = handle.snapshot().revision;
        assert!(record.begin_accepting());
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.revision, voice_revision + 1);
        assert!(matches!(
            snapshot.state,
            RequestState::Processing(ProcessingStage::Accepting { .. })
        ));
        assert_eq!(snapshot.provisional_text(), Some("accepted after voice"));
        assert!(!snapshot.can_cancel());
        assert_eq!(handle.cancel(), CancelResult::TooLate);
        assert!(!record.cancellation.is_cancelled());
        let status = crate::server::status(&snapshot);
        assert_eq!(status["stage"], "accepting");
        assert_eq!(status["text"], "accepted after voice");
        assert_eq!(status["provisional"], true);
        assert_eq!(status["can_cancel"], false);
        record.finish(RequestOutcome::InternalError(
            Arc::<str>::from("rejected").into(),
        ));
        let finished = handle.snapshot();
        assert!(finished.state.is_terminal());
        assert_eq!(finished.provisional_text(), None);
        assert!(!finished.can_cancel());
        assert_eq!(
            crate::server::status(&finished)["provisional"],
            serde_json::Value::Null
        );
        assert_eq!(registry.active_count(), 0);
        let trace = registry.trace(handle.id()).unwrap();
        assert!(trace.transitions.iter().any(|transition| {
            transition.phase == EffectPhase::Accepting && transition.revision == snapshot.revision
        }));
    }

    #[test]
    fn tool_activity_is_provisional_bounded_and_cleared_on_cancellation() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        let record = Arc::clone(&handle.record);
        let args = serde_json::json!({"query": "чай".repeat(2000)});
        let args = args.as_object().unwrap();
        let initial = handle.snapshot().revision;
        record.tool_started("memory_search", args, "Проверю данные");
        assert_eq!(handle.snapshot().revision, initial);
        assert!(handle.snapshot().tool_activity.is_none());

        assert!(record.try_start().is_some());
        record.tool_started("memory_search", args, "Проверю данные");
        let started = handle.snapshot();
        let tool = started.tool_activity.as_ref().unwrap();
        assert_eq!(tool.name.as_ref(), "memory_search");
        assert!(tool.executing);
        assert!(tool.args.len() <= 2048);
        assert!(tool.args.ends_with("..."));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tool.json).unwrap()["tool_call"]["args"],
            "[too large to display]"
        );
        assert!(tool.args.is_char_boundary(tool.args.len()));
        assert_eq!(started.streamed_text.as_deref(), Some("Проверю данные"));

        record.stream_reset();
        assert!(handle.snapshot().tool_activity.as_ref().unwrap().executing);
        record.tool_finished();
        assert!(!handle.snapshot().tool_activity.as_ref().unwrap().executing);
        assert_eq!(handle.cancel(), CancelResult::Requested);
        record.finish_cancelled();
        let terminal = handle.snapshot();
        assert!(matches!(terminal.state, RequestState::Cancelled));
        assert!(terminal.tool_activity.is_none());
    }

    #[test]
    fn completed_tool_activity_does_not_shadow_follow_up_stream_preview() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        let record = Arc::clone(&handle.record);
        assert!(record.try_start().is_some());
        let args = serde_json::json!({"expression": "2+3"});
        record.tool_started("calculator", args.as_object().unwrap(), "Проверяю");
        record.tool_finished();
        record.stream_reset();
        let waiting = handle.snapshot();
        assert_eq!(
            waiting.tool_activity.as_ref().unwrap().name.as_ref(),
            "calculator"
        );
        assert!(!waiting.tool_activity.as_ref().unwrap().executing);
        assert_eq!(waiting.streamed_text.as_deref(), Some("Проверяю"));

        record.stream_delta("{\"segments\":[{\"text\":\"Новый ответ\"}]}");
        let resumed = handle.snapshot();
        assert_eq!(resumed.streamed_text.as_deref(), Some("Новый ответ"));
        assert!(
            resumed.tool_activity.is_none(),
            "finished tool must not shadow a new answer"
        );
        assert!(matches!(
            resumed.state,
            RequestState::Processing(ProcessingStage::GeneratingText)
        ));
        assert_eq!(handle.cancel(), CancelResult::Requested);
        record.finish_cancelled();
        let cancelled = handle.snapshot();
        assert!(cancelled.tool_activity.is_none());
        assert!(cancelled.streamed_json.is_none());
        assert!(cancelled.streamed_text.is_none());
    }

    #[test]
    fn queued_cancel_prevents_start_and_keeps_terminal_handle() {
        let registry = RequestRegistry::new(4);
        let handle = registry.register(
            Box::new(request()),
            RequestId::new(),
            TimelineAdmission {
                selection: selection(),
                context_epoch: 0,
            },
        );
        assert_eq!(handle.cancel(), CancelResult::Cancelled);
        assert!(handle.record.try_start().is_none());
        assert_eq!(registry.active_count(), 0);
        assert!(registry.get(handle.id()).is_none());
        assert!(matches!(handle.snapshot().state, RequestState::Cancelled));
    }
}
