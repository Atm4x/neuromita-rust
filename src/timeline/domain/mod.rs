mod messages;
mod model;
mod projection;
mod store;

pub use messages::{MessageRole, ProjectedMessage, character_messages};
pub use model::{
    BranchId, BranchInfo, CausalLink, CausalRelation, EventDraft, EventKind, KnownEvent, SessionId,
    TimelineError, TimelineFact,
};
pub use projection::{character_view, knows, present_characters};
pub use store::TimelineStore;
