mod handler;
mod model;

pub use crate::identity::CharacterId;
pub(crate) use handler::AcceptanceContext;
pub(crate) use handler::CharacterRegistry;
pub use handler::{AcceptedResponse, CharacterError, CharacterHandle};
pub use model::{
    CharacterContext, CharacterState, CharacterView, GenerationFocus, ReactionCause, ResponseEffect,
};
