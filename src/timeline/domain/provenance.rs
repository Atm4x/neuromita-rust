use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::CharacterId;

/// Session-owned external identity and time fidelity metadata. Canonical
/// Timeline facts remain the only history; this records no event payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionProvenance {
    pub source: ExternalSessionSource,
    pub external_session_id: String,
    pub external_branches: BTreeMap<Uuid, String>,
    pub external_events: BTreeMap<Uuid, ExternalEventProvenance>,
    pub observer_bindings: BTreeMap<String, CharacterId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalSessionSource {
    PythonSessionArchive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEventProvenance {
    pub external_event_id: String,
    pub occurred_at_iso8601: String,
    pub occurred_at_unix_us: i64,
}
