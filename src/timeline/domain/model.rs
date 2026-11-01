use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::identity::{CharacterId, EventId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BranchId(pub Uuid);

impl BranchId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for BranchId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EventKind {
    PlayerMessage,
    CharacterMessage,
    CharacterObservation,
    CharacterJoined,
    CharacterLeft,
    UnityEvent,
    GameStarted,
    GameAction,
    GameEnded,
    PlayerInactive,
    ProactiveOpportunity,
    EventObserved,
    SessionNote,
    MessageHidden,
    MessageRevised,
    MessageRestored,
    CharacterContextReset,
    Checkpoint,
    BranchCreated,
}

impl EventKind {
    pub(crate) fn is_message(self) -> bool {
        matches!(self, Self::PlayerMessage | Self::CharacterMessage)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CausalRelation {
    ReplyTo,
    ReactTo,
    CausedBy,
    Observes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CausalLink {
    pub relation: CausalRelation,
    pub event_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDraft {
    pub id: EventId,
    pub kind: EventKind,
    pub payload: Map<String, Value>,
    pub actor: Option<String>,
    pub observers: BTreeSet<CharacterId>,
    pub links: Vec<CausalLink>,
    pub external_message_id: Option<String>,
    pub occurred_at_unix_ms: u64,
    pub schema_version: u32,
}

impl EventDraft {
    pub fn new(kind: EventKind) -> Self {
        Self {
            id: EventId::new(),
            kind,
            payload: Map::new(),
            actor: None,
            observers: BTreeSet::new(),
            links: Vec::new(),
            external_message_id: None,
            occurred_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            schema_version: 1,
        }
    }

    pub(crate) fn same_intent(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.occurred_at_unix_ms = 0;
        right.occurred_at_unix_ms = 0;
        left.links
            .sort_by_key(|link| (link.relation as u8, link.event_id.0));
        right
            .links
            .sort_by_key(|link| (link.relation as u8, link.event_id.0));
        left == right
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineFact {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub sequence: u64,
    pub draft: EventDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownEvent {
    pub fact: TimelineFact,
    pub learned_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchInfo {
    pub id: BranchId,
    pub session_id: SessionId,
    pub parent: Option<BranchId>,
    pub fork_sequence: u64,
    pub head_sequence: u64,
    pub checkpoint: Option<EventId>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TimelineError {
    #[error("timeline branch or event was not found in this branch world")]
    NotFound,
    #[error("timeline identity is already used for different semantic content")]
    Conflict,
    #[error("timeline branch head advanced since the command was prepared")]
    ConcurrentWrite,
    #[error("invalid timeline operation: {0}")]
    Invalid(&'static str),
}
