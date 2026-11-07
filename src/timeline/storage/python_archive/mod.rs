//! Read-only compatibility boundary for the canonical Python SessionArchive.
//! It does not publish a Rust Timeline session or silently truncate source IDs/time.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

const MAX_ARCHIVE_BYTES: usize = 96 * 1024 * 1024;
const MAX_BRANCHES: usize = 256;
const MAX_EVENTS: usize = 100_000;
const MAX_ASSETS: usize = 256;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PythonArchiveError {
    #[error("invalid Python SessionArchive: {0}")]
    Invalid(&'static str),
    #[error("Python archive JSON encoding failed")]
    Encoding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactPythonTimestamp {
    pub source_iso8601: String,
    pub unix_microseconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalIdentity {
    pub source: String,
    pub candidate_uuid: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonIdentityPlan {
    pub session: ExternalIdentity,
    pub branches: Vec<ExternalIdentity>,
    pub events: Vec<ExternalIdentity>,
    pub external_message_ids: Vec<String>,
    pub observer_ids: Vec<String>,
}

impl PythonIdentityPlan {
    /// Fail before committing if a projected UUID is already occupied in the destination.
    /// The external-to-UUID mapping is only a candidate until persisted transactionally.
    pub fn ensure_unoccupied(
        &self,
        occupied: impl IntoIterator<Item = Uuid>,
    ) -> Result<(), PythonArchiveError> {
        let occupied = occupied.into_iter().collect::<HashSet<_>>();
        if self
            .branches
            .iter()
            .chain(&self.events)
            .any(|id| occupied.contains(&id.candidate_uuid))
            || occupied.contains(&self.session.candidate_uuid)
        {
            return Err(PythonArchiveError::Invalid(
                "native identity already occupied",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct InspectedPythonArchive {
    envelope: PythonEnvelope,
    pub identities: PythonIdentityPlan,
    pub event_times: BTreeMap<String, ExactPythonTimestamp>,
}

impl InspectedPythonArchive {
    /// Re-serialize the Python document without coercing external identity or time precision.
    /// This is a transport-level round-trip, not a Rust Timeline import.
    pub fn to_python_json(&self) -> Result<Vec<u8>, PythonArchiveError> {
        serde_json::to_vec(&self.envelope).map_err(|_| PythonArchiveError::Encoding)
    }

    pub fn external_session_id(&self) -> &str {
        &self.envelope.session_id
    }

    pub fn event_count(&self) -> usize {
        self.envelope.events.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonEnvelope {
    archive_version: u32,
    session_id: String,
    root_branch_id: String,
    branches: Vec<PythonBranch>,
    events: Vec<PythonEvent>,
    #[serde(default)]
    operations: Vec<PythonOperation>,
    #[serde(default)]
    assets: Vec<PythonAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonBranch {
    branch_id: String,
    session_id: String,
    parent_branch_id: Option<String>,
    fork_sequence: u64,
    head_sequence: u64,
    checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonEvent {
    event_id: String,
    session_id: String,
    branch_id: String,
    sequence: u64,
    kind: String,
    payload: Value,
    actor_id: Option<String>,
    observers: Vec<String>,
    links: Vec<(String, String)>,
    occurred_at: String,
    schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonOperation {
    branch_id: String,
    key: String,
    intent: Value,
    event_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PythonAsset {
    asset_id: String,
    media_type: String,
    base64: String,
}

mod assets;
mod binding;
mod conversion;
pub use conversion::PreparedPythonCanonical;
mod validation;
use assets::validate_assets;
pub use binding::BoundPythonArchive;
use validation::*;

/// Validate a Python archive into a detached transport plan. No canonical Rust
/// Timeline or AssetStore writes occur. Original external keys and microsecond
/// timestamps remain intact; candidate UUIDs are *not* persistent mappings.
pub fn inspect_python_archive(bytes: &[u8]) -> Result<InspectedPythonArchive, PythonArchiveError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(PythonArchiveError::Invalid("archive byte limit"));
    }
    let envelope: PythonEnvelope = serde_json::from_slice(bytes)
        .map_err(|_| PythonArchiveError::Invalid("archive JSON or unknown field"))?;
    if envelope.archive_version != 1 {
        return Err(PythonArchiveError::Invalid(
            "unknown Python archive version",
        ));
    }
    checked_id(&envelope.session_id)?;
    checked_id(&envelope.root_branch_id)?;
    if envelope.branches.is_empty()
        || envelope.branches.len() > MAX_BRANCHES
        || envelope.events.len() > MAX_EVENTS
        || envelope.assets.len() > MAX_ASSETS
    {
        return Err(PythonArchiveError::Invalid("archive collection limit"));
    }
    let branches = envelope
        .branches
        .iter()
        .map(|branch| {
            checked_id(&branch.branch_id)?;
            checked_id(&branch.session_id)?;
            if let Some(parent) = &branch.parent_branch_id {
                checked_id(parent)?;
            }
            if let Some(checkpoint) = &branch.checkpoint_id {
                checked_id(checkpoint)?;
            }
            if branch.session_id != envelope.session_id
                || branch.fork_sequence > branch.head_sequence
            {
                return Err(PythonArchiveError::Invalid("branch metadata"));
            }
            Ok((branch.branch_id.as_str(), branch))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    if branches.len() != envelope.branches.len() {
        return Err(PythonArchiveError::Invalid("duplicate branch"));
    }
    let root = branches
        .get(envelope.root_branch_id.as_str())
        .ok_or(PythonArchiveError::Invalid("missing root"))?;
    if root.parent_branch_id.is_some()
        || root.fork_sequence != 0
        || root.checkpoint_id.is_some()
        || envelope
            .branches
            .iter()
            .filter(|b| b.parent_branch_id.is_none())
            .count()
            != 1
    {
        return Err(PythonArchiveError::Invalid("invalid root"));
    }
    let events = envelope
        .events
        .iter()
        .map(|event| {
            checked_id(&event.event_id)?;
            if event.session_id != envelope.session_id
                || !branches.contains_key(event.branch_id.as_str())
                || event.sequence == 0
                || !event.payload.is_object()
                || event.schema_version != 1
                || !known_python_kind(&event.kind)
            {
                return Err(PythonArchiveError::Invalid("event metadata"));
            }
            if let Some(actor) = &event.actor_id {
                checked_id(actor)?;
            }
            let mut observers = HashSet::new();
            for observer in &event.observers {
                checked_id(observer)?;
                if !observers.insert(observer) {
                    return Err(PythonArchiveError::Invalid("duplicate observer"));
                }
            }
            if matches!(
                event.kind.as_str(),
                "PlayerMessage" | "CharacterMessage" | "CharacterObservation"
            ) && event.actor_id.is_none()
            {
                return Err(PythonArchiveError::Invalid("missing message actor"));
            }
            if matches!(
                event.kind.as_str(),
                "CharacterMessage" | "CharacterObservation"
            ) && !observers.contains(event.actor_id.as_ref().expect("checked above"))
            {
                return Err(PythonArchiveError::Invalid(
                    "actor not observing own message",
                ));
            }
            let mut links = HashSet::new();
            for (relation, target) in &event.links {
                checked_id(target)?;
                if !matches!(
                    relation.as_str(),
                    "reply_to" | "react_to" | "caused_by" | "observes"
                ) || !links.insert((relation, target))
                {
                    return Err(PythonArchiveError::Invalid("causal link"));
                }
            }
            Ok((event.event_id.as_str(), event))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    if events.len() != envelope.events.len() {
        return Err(PythonArchiveError::Invalid("duplicate event"));
    }
    let mut ordered = Vec::new();
    let mut done = HashSet::new();
    let mut pending = envelope.branches.iter().collect::<Vec<_>>();
    while !pending.is_empty() {
        let index = pending
            .iter()
            .position(|branch| {
                branch
                    .parent_branch_id
                    .as_ref()
                    .is_none_or(|id| done.contains(id.as_str()))
            })
            .ok_or(PythonArchiveError::Invalid(
                "cyclic or missing branch parent",
            ))?;
        let branch = pending.remove(index);
        ordered.push(branch);
        done.insert(branch.branch_id.as_str());
    }
    let mut worlds = HashMap::<&str, Vec<&PythonEvent>>::new();
    for branch in ordered {
        let inherited = if let Some(parent_id) = &branch.parent_branch_id {
            let parent_world = worlds
                .get(parent_id.as_str())
                .ok_or(PythonArchiveError::Invalid("parent world unavailable"))?;
            let checkpoint = branch
                .checkpoint_id
                .as_deref()
                .ok_or(PythonArchiveError::Invalid("missing fork checkpoint"))?;
            if !parent_world.iter().any(|event| {
                event.event_id == checkpoint
                    && event.kind == "Checkpoint"
                    && event.sequence == branch.fork_sequence
            }) {
                return Err(PythonArchiveError::Invalid("fork checkpoint mismatch"));
            }
            parent_world
                .iter()
                .copied()
                .filter(|event| event.sequence <= branch.fork_sequence)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut additions = envelope
            .events
            .iter()
            .filter(|event| event.branch_id == branch.branch_id)
            .collect::<Vec<_>>();
        additions.sort_by_key(|event| event.sequence);
        if additions.len() as u64 != branch.head_sequence - branch.fork_sequence
            || additions
                .iter()
                .enumerate()
                .any(|(index, event)| event.sequence != branch.fork_sequence + index as u64 + 1)
        {
            return Err(PythonArchiveError::Invalid("noncontiguous branch sequence"));
        }
        let mut available = inherited
            .iter()
            .map(|event| (event.event_id.as_str(), *event))
            .collect::<HashMap<_, _>>();
        let mut knowledge = HashMap::new();
        let mut message_ids = HashSet::new();
        for event in &inherited {
            observe_python(event, &mut knowledge);
            if is_history(&event.kind)
                && let Some(id) = event.payload.get("message_id").and_then(Value::as_str)
                && !id.is_empty()
            {
                message_ids.insert(id);
            }
        }
        for (index, event) in additions.iter().enumerate() {
            if available.contains_key(event.event_id.as_str()) {
                return Err(PythonArchiveError::Invalid("reused event identity"));
            }
            if is_history(&event.kind)
                && let Some(id) = event.payload.get("message_id").and_then(Value::as_str)
                && !id.is_empty()
            {
                checked_id(id)?;
                if !message_ids.insert(id) {
                    return Err(PythonArchiveError::Invalid(
                        "duplicate external message identity",
                    ));
                }
            }
            if branch.parent_branch_id.is_some() && index == 0 {
                if event.kind != "BranchCreated"
                    || event.payload != json!({"checkpoint_id": branch.checkpoint_id})
                    || event.actor_id.is_some()
                    || !event.observers.is_empty()
                    || event.links
                        != vec![(
                            "caused_by".into(),
                            branch.checkpoint_id.clone().expect("checkpoint exists"),
                        )]
                {
                    return Err(PythonArchiveError::Invalid("invalid fork marker"));
                }
            } else if event.kind == "BranchCreated" {
                return Err(PythonArchiveError::Invalid("unexpected fork marker"));
            } else {
                validate_python_event(event, &available, &knowledge)?;
            }
            available.insert(event.event_id.as_str(), event);
            observe_python(event, &mut knowledge);
        }
        let mut world = inherited;
        world.extend(additions);
        worlds.insert(branch.branch_id.as_str(), world);
    }
    let mut operations = HashSet::new();
    let mut covered = HashSet::new();
    for op in &envelope.operations {
        checked_id(&op.branch_id)?;
        checked_id(&op.key)?;
        if !branches.contains_key(op.branch_id.as_str())
            || !operations.insert((&op.branch_id, &op.key))
            || !op.intent.is_object()
        {
            return Err(PythonArchiveError::Invalid("operation identity"));
        }
        let mut selected = Vec::new();
        let mut previous = None;
        for id in &op.event_ids {
            let event = events
                .get(id.as_str())
                .ok_or(PythonArchiveError::Invalid("receipt event missing"))?;
            if event.branch_id != op.branch_id
                || !covered.insert(id.as_str())
                || previous.is_some_and(|number| event.sequence != number + 1)
            {
                return Err(PythonArchiveError::Invalid("receipt event sequence"));
            }
            previous = Some(event.sequence);
            selected.push(canonical_intent(event));
        }
        if op.intent
            != json!({"command": op.intent.get("command").cloned().unwrap_or_else(|| json!({})), "events": selected})
        {
            return Err(PythonArchiveError::Invalid("receipt intent mismatch"));
        }
    }
    if envelope
        .events
        .iter()
        .any(|event| event.kind != "BranchCreated" && !covered.contains(event.event_id.as_str()))
    {
        return Err(PythonArchiveError::Invalid(
            "event missing operation receipt",
        ));
    }
    validate_assets(&envelope)?;
    let mut event_times = BTreeMap::new();
    for event in &envelope.events {
        event_times.insert(event.event_id.clone(), exact_time(&event.occurred_at)?);
    }
    let mut uuids = HashSet::new();
    let session = mapped("session", &envelope.session_id, &envelope.session_id);
    uuids.insert(session.candidate_uuid);
    let mut identities = PythonIdentityPlan {
        session,
        branches: Vec::new(),
        events: Vec::new(),
        external_message_ids: Vec::new(),
        observer_ids: Vec::new(),
    };
    for branch in &envelope.branches {
        let identity = mapped("branch", &envelope.session_id, &branch.branch_id);
        if !uuids.insert(identity.candidate_uuid) {
            return Err(PythonArchiveError::Invalid("identity collision"));
        }
        identities.branches.push(identity);
    }
    for event in &envelope.events {
        let identity = mapped("event", &envelope.session_id, &event.event_id);
        if !uuids.insert(identity.candidate_uuid) {
            return Err(PythonArchiveError::Invalid("identity collision"));
        }
        identities.events.push(identity);
    }
    identities.branches.sort_by(|a, b| a.source.cmp(&b.source));
    identities.events.sort_by(|a, b| a.source.cmp(&b.source));
    identities.external_message_ids = envelope
        .events
        .iter()
        .filter_map(|event| event.payload.get("message_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    identities.observer_ids = envelope
        .events
        .iter()
        .flat_map(|event| event.observers.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(InspectedPythonArchive {
        envelope,
        identities,
        event_times,
    })
}
