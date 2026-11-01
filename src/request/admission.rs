use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::mpsc;

use super::{Message, RequestDiscovery, RequestHandle, RequestId, RequestRegistry, SubmitError};
use crate::identity::CharacterId;

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
