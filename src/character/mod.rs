mod handler;
mod lexical_dedup;
pub mod memory;
pub(crate) mod memory_access;
mod model;
mod portable_access;
pub(crate) mod projection;
pub mod state;
mod variables_access;

pub use crate::identity::CharacterId;
pub(crate) use handler::AcceptanceContext;
pub(crate) use handler::CharacterRegistry;
pub(crate) use handler::ReminderBridge;
pub use handler::{AcceptedResponse, CharacterError, CharacterHandle};
pub use model::{
    CharacterContext, CharacterDefinition, CharacterState, CharacterView, GenerationFocus,
    ReactionCause, character_catalog, character_definition,
};

pub use memory_access::{CharacterMemorySnapshot, MemoryEditResult};
pub use portable_access::{CharacterPortableResult, CharacterPortableSnapshot};
pub use variables_access::CharacterVariablesSnapshot;
