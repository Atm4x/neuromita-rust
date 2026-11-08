use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use super::{
    DiagnosticScope, DiagnosticsAccess, GenerationCommand, RequestDiscovery, RequestHandle,
    RequestRegistry, SubmitError,
};
use crate::{
    generation::PresetCatalog,
    identity::{CharacterId, EventId, RequestId},
    interaction::{TurnIntent, UserInput},
    prompts::PromptCatalog,
    request::TimelineFence,
    settings::SettingsOwner,
    timeline::{
        RegenerateAdmissionReceipt, RegenerateConfigurationReference, RegenerateSource,
        conversation::ConversationMessage, domain::BranchId, selection::TimelineAdmission,
    },
};

pub(crate) trait RegenerateCancellation: Send + Sync {
    fn abandon_regenerate(
        &self,
        request: RequestId,
        admission: TimelineAdmission,
    ) -> Result<(), String>;
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
        self.send_with_regeneration(request, Some((source, fence, child)))
    }

    fn send_with_regeneration(
        &self,
        request: impl GenerationCommand + 'static,
        regeneration: Option<(RegenerateSource, TimelineFence, BranchId)>,
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
        let admission = if let Some((source, fence, child)) = regeneration {
            if request.character_id() != source.character
                || request.intent() != TurnIntent::Reply(source.original_input)
            {
                return Err(SubmitError::InvalidTargetEvent);
            }
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
                configuration: RegenerateConfigurationReference {
                    settings_revision: settings.revision,
                    preset_id: preset_id.0,
                    provider_model: preset.template().model().to_owned(),
                    prompt_set: prompt.as_ref().map(|snapshot| snapshot.name().to_owned()),
                },
            };
            self.ingress
                .admit_regenerate(source, fence, child, &receipt)?
        } else {
            self.ingress.admit(
                request.character_id(),
                request.intent(),
                request.timeline_fence(),
            )?
        };
        let handle = self.registry.register_with_regenerate_cancellation(
            Box::new(request),
            id,
            admission,
            Some(settings),
            Some(preset),
            prompt,
            regeneration.map(|_| Arc::clone(&self.ingress) as Arc<dyn RegenerateCancellation>),
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
