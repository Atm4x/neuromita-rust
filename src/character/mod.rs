mod handler;
pub mod memory;
pub(crate) mod memory_access;
mod model;
pub(crate) mod projection;
pub mod state;

pub use crate::identity::CharacterId;
pub(crate) use handler::AcceptanceContext;
pub(crate) use handler::CharacterRegistry;
pub use handler::{AcceptedResponse, CharacterError, CharacterHandle};
pub use model::{CharacterContext, CharacterState, CharacterView, GenerationFocus, ReactionCause};

pub use memory_access::{CharacterMemorySnapshot, MemoryEditResult};
