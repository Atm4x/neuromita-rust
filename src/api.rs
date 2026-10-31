use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterId},
    request::{
        InitiateRequest, Message, ReactRequest, ReplyRequest, RequestDiscovery, RequestHandle,
        RequestId, RequestRegistry, SubmitError,
    },
    runtime::{self, ApiUnavailable},
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

pub(crate) struct ChatApiState {
    accepting: Mutex<bool>,
    pub(crate) registry: Arc<RequestRegistry>,
    handlers: HashMap<CharacterId, mpsc::Sender<RequestId>>,
}

impl ChatApiState {
    pub(crate) fn new(
        registry: Arc<RequestRegistry>,
        handlers: HashMap<CharacterId, mpsc::Sender<RequestId>>,
    ) -> Self {
        Self {
            accepting: Mutex::new(true),
            registry,
            handlers,
        }
    }

    pub(crate) fn close(&self) {
        *self.accepting.lock().expect("admission mutex poisoned") = false;
        self.registry.cancel_all();
    }
    pub(crate) fn send(
        &self,
        request: impl Message + 'static,
    ) -> Result<RequestHandle, SubmitError> {
        let accepting = self.accepting.lock().expect("admission mutex poisoned");
        if !*accepting {
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
        let handle = self.registry.register(Box::new(request));
        permit.send(handle.id());
        Ok(handle)
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
}

impl ChatApi {
    pub fn reply(request: ReplyRequest) -> Result<RequestHandle, SubmitError> {
        Self::send(request)
    }
    pub fn react(request: ReactRequest) -> Result<RequestHandle, SubmitError> {
        Self::send(request)
    }
    pub fn initiate(request: InitiateRequest) -> Result<RequestHandle, SubmitError> {
        Self::send(request)
    }
    pub fn send(request: impl Message + 'static) -> Result<RequestHandle, SubmitError> {
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
