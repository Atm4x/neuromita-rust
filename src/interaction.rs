use std::sync::Arc;

use thiserror::Error;

use crate::identity::EventId;

#[derive(Debug, Error)]
pub enum InputError {
    #[error("message must not be empty")]
    Empty,
    #[error("message exceeds 64 KiB")]
    TooLarge,
}

#[derive(Debug, Clone)]
pub struct UserInput {
    event_id: EventId,
    text: Arc<str>,
}

impl UserInput {
    pub fn new(text: String) -> Result<Self, InputError> {
        if text.trim().is_empty() {
            return Err(InputError::Empty);
        }
        if text.len() > 64 * 1024 {
            return Err(InputError::TooLarge);
        }
        Ok(Self {
            event_id: EventId::new(),
            text: Arc::from(text),
        })
    }
    pub fn event_id(&self) -> EventId {
        self.event_id
    }
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnIntent {
    Reply,
    React(EventId),
    Initiate,
}
