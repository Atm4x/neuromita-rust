use std::sync::Arc;

use tokio::sync::watch;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterId},
    interaction::UserInput,
    request::{
        GenerationCommand, InitiateRequest, ReactRequest, ReplyRequest, RequestDiscovery,
        RequestHandle, RequestId, SubmitError,
    },
    runtime::{self, ApiUnavailable},
    settings::{
        SectionSnapshot, SettingsApi, SettingsError, SettingsOwner, SettingsSection,
        SettingsSnapshot,
    },
    timeline::EventId,
};

pub struct CharacterApi;
impl CharacterApi {
    pub fn get(id: CharacterId) -> Result<CharacterHandle, CharacterError> {
        runtime::current()
            .ok()
            .and_then(|bindings| bindings.characters.get(id))
            .ok_or(CharacterError::Unavailable)
    }
}

pub struct ChatApi;

impl ChatApi {
    pub fn publish_conversation_message(
        message: crate::timeline::conversation::ConversationMessage,
    ) -> Result<crate::identity::EventId, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .publish_conversation_message(message)
    }

    pub fn publish_message(
        character: CharacterId,
        input: UserInput,
    ) -> Result<EventId, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .publish_message(character, input)
    }

    pub fn reply(request: ReplyRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    pub fn react(request: ReactRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    pub fn initiate(request: InitiateRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    fn submit(request: impl GenerationCommand + 'static) -> Result<RequestHandle, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .send(request)
    }
    pub fn observe_requests() -> Result<RequestDiscovery, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .observe_requests()
    }
    pub fn active_request(id: RequestId) -> Result<Option<RequestHandle>, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .active_request(id)
    }
    pub fn trace(id: RequestId) -> Result<Option<Arc<crate::request::RequestTrace>>, SubmitError> {
        Ok(runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .registry
            .trace(id))
    }
}

impl From<ApiUnavailable> for SubmitError {
    fn from(error: ApiUnavailable) -> Self {
        match error {
            ApiUnavailable::Uninitialized => Self::Uninitialized,
            ApiUnavailable::Stopped => Self::Stopped,
        }
    }
}

impl SettingsApi {
    fn owner() -> Result<Arc<SettingsOwner>, SettingsError> {
        Ok(Arc::clone(
            &runtime::current().map_err(SettingsError::from)?.settings,
        ))
    }
    pub fn snapshot<T: SettingsSection>() -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.snapshot()
    }
    pub fn snapshot_all() -> Result<Arc<SettingsSnapshot>, SettingsError> {
        Self::owner()?.snapshot_all()
    }
    pub fn observe() -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        Self::owner()?.observe()
    }
    pub fn replace<T: SettingsSection>(
        expected_revision: u64,
        value: T,
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.replace(expected_revision, value)
    }
    /// The edit must be short and must not block or re-enter SettingsApi.
    pub fn edit<T: SettingsSection>(
        edit: impl FnOnce(&mut T),
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.edit(edit)
    }
    pub async fn flush() -> Result<(), SettingsError> {
        Self::owner()?.flush().await
    }
    pub fn retry_save() -> Result<(), SettingsError> {
        Self::owner()?.request_save()
    }
}

impl From<ApiUnavailable> for SettingsError {
    fn from(error: ApiUnavailable) -> Self {
        match error {
            ApiUnavailable::Uninitialized => Self::Uninitialized,
            ApiUnavailable::Stopped => Self::Stopped,
        }
    }
}
