use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use super::{
    DiagnosticScope, DiagnosticsAccess, GenerationCommand, RegenerateProtocol, RequestDiscovery,
    RequestHandle, RequestRegistry, SubmitError,
};
use crate::{
    generation::PresetCatalog,
    identity::{CharacterId, EventId, RequestId},
    interaction::{TurnIntent, UserInput},
    prompts::PromptCatalog,
    request::TimelineFence,
    settings::SettingsOwner,
    timeline::{
        RegenerateAdmissionReceipt, RegenerateConfigurationReference, RegenerateRecoveryOutcome,
        RegenerateRecoverySnapshot, RegenerateSource,
        conversation::ConversationMessage,
        domain::{BranchId, TimelineError},
        selection::TimelineAdmission,
    },
};

#[derive(Debug, Clone, Copy)]
enum Regeneration {
    LegacyFork(RegenerateSource, TimelineFence, BranchId),
    SameBranch(RegenerateSource, TimelineFence),
}

pub(crate) trait RegenerateCancellation: Send + Sync {
    fn abandon_regenerate(
        &self,
        request: RequestId,
        admission: TimelineAdmission,
    ) -> Result<(), TimelineError>;

    fn inspect_regenerate(
        &self,
        child: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoverySnapshot, TimelineError>;

    fn abandon_same_branch_regenerate(
        &self,
        request: RequestId,
        admission: TimelineAdmission,
    ) -> Result<(), TimelineError>;

    fn inspect_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, TimelineError>;

    fn abandon_orphaned_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, TimelineError>;
}

pub(crate) trait RequestIngress: RegenerateCancellation {
    fn publish_conversation_message(
        &self,
        message: ConversationMessage,
    ) -> Result<EventId, SubmitError>;
    fn publish_message(
        &self,
        character: CharacterId,
        input: &UserInput,
    ) -> Result<EventId, SubmitError>;

    fn admit(
        &self,
        character: CharacterId,
        intent: TurnIntent,
        fence: Option<TimelineFence>,
    ) -> Result<TimelineAdmission, SubmitError>;

    fn admit_regenerate(
        &self,
        source: RegenerateSource,
        fence: TimelineFence,
        child: BranchId,
        receipt: &RegenerateAdmissionReceipt,
    ) -> Result<TimelineAdmission, SubmitError>;

    fn admit_same_branch_regenerate(
        &self,
        source: RegenerateSource,
        fence: TimelineFence,
        request: RequestId,
        configuration: RegenerateConfigurationReference,
    ) -> Result<TimelineAdmission, SubmitError>;
}

pub(crate) struct ChatApiState {
    accepting: Mutex<bool>,
    pub(crate) registry: Arc<RequestRegistry>,
    handlers: HashMap<CharacterId, mpsc::Sender<RequestId>>,
    ingress: Arc<dyn RequestIngress>,
    settings: Arc<SettingsOwner>,
    presets: Arc<PresetCatalog>,
    prompts: Option<Arc<PromptCatalog>>,
}

impl ChatApiState {
    pub(crate) fn new(
        registry: Arc<RequestRegistry>,
        handlers: HashMap<CharacterId, mpsc::Sender<RequestId>>,
        ingress: Arc<dyn RequestIngress>,
        settings: Arc<SettingsOwner>,
        presets: Arc<PresetCatalog>,
        prompts: Option<Arc<PromptCatalog>>,
    ) -> Self {
        Self {
            accepting: Mutex::new(true),
            registry,
            handlers,
            ingress,
            settings,
            presets,
            prompts,
        }
    }

    pub(crate) fn close(&self) {
        *self.accepting.lock().expect("admission mutex poisoned") = false;
        self.registry.cancel_all();
        for (request, result) in self.reconcile_pending_terminals() {
            match result {
                Ok(super::RegenerateTerminalReconciliation::Abandoned) => {}
                Ok(super::RegenerateTerminalReconciliation::AcceptedElsewhere(event)) => {
                    tracing::warn!(%request, event = %event.0, "regenerate accepted by another owner; volatile completion requires explicit terminal projection");
                }
                Err(error) => {
                    tracing::warn!(%request, %error, "regenerate terminal remains unresolved at shutdown");
                }
            }
        }
    }
    /// Read-only orphan inspection; does not recreate a request or dispatch a provider.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn inspect_orphaned_regenerate(
        &self,
        child: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoverySnapshot, TimelineError> {
        self.ingress.inspect_regenerate(child, request)
    }

    /// Read-only same-branch orphan inspection. Unknown is never permission to replay.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn inspect_orphaned_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, TimelineError> {
        self.ingress.inspect_same_branch_regenerate(branch, request)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn abandon_orphaned_same_branch_regenerate(
        &self,
        branch: BranchId,
        request: RequestId,
    ) -> Result<RegenerateRecoveryOutcome, TimelineError> {
        let _admission = self.accepting.lock().expect("admission mutex poisoned");
        if self.registry.get(request).is_some() {
            return Err(TimelineError::Conflict);
        }
        self.ingress
            .abandon_orphaned_same_branch_regenerate(branch, request)
    }

    /// Operations-level retry, never re-enqueues or invokes a provider.
    pub(crate) fn reconcile_pending_terminals(
        &self,
    ) -> Vec<(
        RequestId,
        Result<super::RegenerateTerminalReconciliation, String>,
    )> {
        self.registry.reconcile_pending_terminals()
    }

    pub(crate) fn send(
        &self,
        request: impl GenerationCommand + 'static,
    ) -> Result<RequestHandle, SubmitError> {
        self.send_with_regeneration(request, None)
    }

    /// Internal-only submission: no UI/public API until restart-safe durable
    /// Regenerate admission and cancellation reconciliation are implemented.
    #[allow(dead_code)]
    pub(crate) fn regenerate(
        &self,
        source: RegenerateSource,
        fence: TimelineFence,
        child: BranchId,
    ) -> Result<RequestHandle, SubmitError> {
        let request = super::ReplyRequest::new(
            source.character,
            source.original_input,
            super::RequestSource::Presenter,
        );
        self.send_with_regeneration(
            request,
            Some(Regeneration::LegacyFork(source, fence, child)),
        )
    }

    /// Backend-only same-branch admission. No public/UI entry point yet.
    #[allow(dead_code)]
    pub(crate) fn regenerate_same_branch(
        &self,
        source: RegenerateSource,
        fence: TimelineFence,
    ) -> Result<RequestHandle, SubmitError> {
        let request = super::ReplyRequest::new(
            source.character,
            source.original_input,
            super::RequestSource::Presenter,
        );
        self.send_with_regeneration(request, Some(Regeneration::SameBranch(source, fence)))
    }

    fn send_with_regeneration(
        &self,
        request: impl GenerationCommand + 'static,
        regeneration: Option<Regeneration>,
    ) -> Result<RequestHandle, SubmitError> {
        if !*self.accepting.lock().expect("admission mutex poisoned") {
            return Err(SubmitError::Stopped);
        }
        let handler = self
            .handlers
            .get(&request.character_id())
            .ok_or(SubmitError::CharacterUnavailable)?;
        let permit = handler.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SubmitError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => SubmitError::QueueClosed,
        })?;
        let settings = self
            .settings
            .snapshot_all()
            .map_err(|_| SubmitError::Stopped)?;
        let preset_id = settings
            .values
            .selected_preset(request.character_id(), request.intent());
        let preset = self
            .presets
            .get(preset_id)
            .map_err(|_| SubmitError::PresetUnavailable)?
            .ok_or(SubmitError::PresetUnavailable)?;
        let selected = settings
            .values
            .characters
            .for_character(request.character_id())
            .prompt_set
            .as_deref();
        let prompt = self
            .prompts
            .as_ref()
            .map(|catalog| catalog.capture(request.character_id(), selected))
            .transpose()
            .map_err(|error| SubmitError::PromptUnavailable(error.to_string()))?
            .flatten();
        // Resource I/O happens before the admission mutex is acquired.
        if matches!(request.intent(), TurnIntent::React(_))
            && let Some(snapshot) = &prompt
            && !snapshot.has_template("react_template.txt")
        {
            return Err(SubmitError::PromptUnavailable(format!(
                "{}/react_template.txt",
                snapshot.name()
            )));
        }
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        let id = RequestId::new();
        let configuration = RegenerateConfigurationReference {
            settings_revision: settings.revision,
            preset_id: preset_id.0,
            provider_model: preset.template().model().to_owned(),
            prompt_set: prompt.as_ref().map(|snapshot| snapshot.name().to_owned()),
        };
        let admission = if let Some(regeneration) = regeneration {
            let source = match regeneration {
                Regeneration::LegacyFork(source, _, _) | Regeneration::SameBranch(source, _) => {
                    source
                }
            };
            if request.character_id() != source.character
                || request.intent() != TurnIntent::Reply(source.original_input)
            {
                return Err(SubmitError::InvalidTargetEvent);
            }
            match regeneration {
                Regeneration::SameBranch(source, fence) => self
                    .ingress
                    .admit_same_branch_regenerate(source, fence, id, configuration)?,
                Regeneration::LegacyFork(source, fence, child) => {
                    let receipt = RegenerateAdmissionReceipt {
                        request: id,
                        accepted_request: source.accepted_request,
                        accepted_message: source.accepted_message,
                        original_input: source.original_input,
                        character: source.character,
                        parent_branch: fence.selection.branch_id,
                        parent_head: fence.at.through,
                        parent_generation: fence.selection.generation,
                        child_branch: child,
                        configuration,
                    };
                    self.ingress
                        .admit_regenerate(source, fence, child, &receipt)?
                }
            }
        } else {
            self.ingress.admit(
                request.character_id(),
                request.intent(),
                request.timeline_fence(),
            )?
        };
        let protocol = regeneration.map(|kind| match kind {
            Regeneration::LegacyFork(..) => RegenerateProtocol::LegacyFork,
            Regeneration::SameBranch(..) => RegenerateProtocol::SameBranch,
        });
        let handle = self.registry.register_with_regenerate_protocol(
            Box::new(request),
            id,
            admission,
            Some(settings),
            Some(preset),
            prompt,
            protocol,
            protocol.map(|_| Arc::clone(&self.ingress) as Arc<dyn RegenerateCancellation>),
        );
        permit.send(id);
        Ok(handle)
    }

    pub(crate) fn publish_conversation_message(
        &self,
        message: ConversationMessage,
    ) -> Result<EventId, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        self.ingress.publish_conversation_message(message)
    }

    pub(crate) fn publish_message(
        &self,
        character: CharacterId,
        input: UserInput,
    ) -> Result<EventId, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        self.ingress.publish_message(character, &input)
    }

    pub(crate) fn observe_requests(&self) -> Result<RequestDiscovery, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
            return Err(SubmitError::Stopped);
        }
        Ok(self.registry.observe())
    }

    pub(crate) fn active_request(
        &self,
        id: RequestId,
    ) -> Result<Option<RequestHandle>, SubmitError> {
        if !*self.accepting.lock().expect("admission mutex poisoned") {
            return Err(SubmitError::Stopped);
        }
        Ok(self.registry.get(id).map(|record| record.handle()))
    }

    pub(crate) fn diagnostics(
        &self,
        scope: DiagnosticScope,
    ) -> Result<DiagnosticsAccess, SubmitError> {
        if !*self.accepting.lock().expect("admission mutex poisoned") {
            return Err(SubmitError::Stopped);
        }
        if !self.handlers.contains_key(&scope.character_id()) {
            return Err(SubmitError::CharacterUnavailable);
        }
        Ok(self.registry.diagnostics_access(scope))
    }
}
