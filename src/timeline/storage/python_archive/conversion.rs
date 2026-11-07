use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use super::{BoundPythonArchive, PythonArchiveError, PythonEvent};
use crate::identity::{CharacterId, EventId};
use crate::timeline::domain::{
    Branch, BranchId, BranchInfo, CausalLink, CausalRelation, EventDraft, EventKind, Receipt,
    SessionId, SessionProvenance, TimelineFact, TimelineStore,
};

/// Detached canonical facts and receipts. No Timeline/SQLite/AssetStore writes take place.
/// Unsupported Python semantics cause a hard error rather than a lossy import.
#[derive(Debug)]
pub struct PreparedPythonCanonical {
    session: SessionId,
    branches: Vec<Branch>,
    provenance: SessionProvenance,
}

impl PreparedPythonCanonical {
    pub fn session_id(&self) -> SessionId {
        self.session
    }

    pub fn fact(&self, id: EventId) -> Option<&TimelineFact> {
        self.branches
            .iter()
            .flat_map(|branch| &branch.events)
            .find(|fact| fact.draft.id == id)
    }

    pub fn receipt_count(&self) -> usize {
        self.branches
            .iter()
            .map(|branch| branch.receipts.len())
            .sum()
    }

    pub fn provenance(&self) -> &SessionProvenance {
        &self.provenance
    }
}

impl BoundPythonArchive<'_> {
    /// Transform only known semantically equivalent Python events to native drafts
    /// and receipts. Verify the detached result with the *native* replay validator.
    /// Python `EventObserved`, presentation revisions and binary media require
    /// separate semantic translation; they are never flattened or discarded.
    pub fn prepare_canonical(&self) -> Result<PreparedPythonCanonical, PythonArchiveError> {
        let id_map = self
            .source
            .identities
            .events
            .iter()
            .map(|id| (id.source.as_str(), EventId(id.candidate_uuid)))
            .collect::<HashMap<_, _>>();
        let branch_map = self
            .source
            .identities
            .branches
            .iter()
            .map(|id| (id.source.as_str(), BranchId(id.candidate_uuid)))
            .collect::<HashMap<_, _>>();
        let session = self.native_session();
        let event_lookup = self
            .source
            .envelope
            .events
            .iter()
            .map(|event| (event.event_id.as_str(), event))
            .collect::<HashMap<_, _>>();
        let mut drafts = HashMap::new();
        for event in &self.source.envelope.events {
            let draft = native_draft(
                event,
                &id_map,
                &self.observers,
                &branch_map,
                &self.source.envelope.branches,
            )?;
            drafts.insert(event.event_id.as_str(), draft);
        }
        let mut receipts: HashMap<&str, HashMap<String, Receipt>> = HashMap::new();
        for operation in &self.source.envelope.operations {
            let mut result = Vec::with_capacity(operation.event_ids.len());
            let mut receipt_drafts = Vec::with_capacity(operation.event_ids.len());
            for source_id in &operation.event_ids {
                let event = event_lookup
                    .get(source_id.as_str())
                    .ok_or(PythonArchiveError::Invalid("receipt source event missing"))?;
                let draft = drafts
                    .get(source_id.as_str())
                    .ok_or(PythonArchiveError::Invalid("receipt native event missing"))?
                    .clone();
                result.push(TimelineFact {
                    session_id: session,
                    branch_id: *branch_map
                        .get(operation.branch_id.as_str())
                        .ok_or(PythonArchiveError::Invalid("receipt branch missing"))?,
                    sequence: event.sequence,
                    draft: draft.clone(),
                });
                receipt_drafts.push(draft);
            }
            let command = operation
                .intent
                .get("command")
                .cloned()
                .ok_or(PythonArchiveError::Invalid("receipt command missing"))?;
            if !command.is_object() {
                return Err(PythonArchiveError::Invalid("receipt command not an object"));
            }
            receipts
                .entry(operation.branch_id.as_str())
                .or_default()
                .insert(
                    operation.key.clone(),
                    Receipt::new(command, receipt_drafts, result),
                );
        }
        let mut branches = Vec::with_capacity(self.source.envelope.branches.len());
        for branch in &self.source.envelope.branches {
            let id =
                *branch_map
                    .get(branch.branch_id.as_str())
                    .ok_or(PythonArchiveError::Invalid(
                        "native branch identity missing",
                    ))?;
            let events = self
                .source
                .envelope
                .events
                .iter()
                .filter(|event| event.branch_id == branch.branch_id)
                .map(|event| {
                    Ok(TimelineFact {
                        session_id: session,
                        branch_id: id,
                        sequence: event.sequence,
                        draft: drafts
                            .get(event.event_id.as_str())
                            .ok_or(PythonArchiveError::Invalid("native event missing"))?
                            .clone(),
                    })
                })
                .collect::<Result<Vec<_>, PythonArchiveError>>()?;
            let mut events = events;
            events.sort_by_key(|fact| fact.sequence);
            branches.push(Branch {
                info: BranchInfo {
                    id,
                    session_id: session,
                    parent: branch
                        .parent_branch_id
                        .as_ref()
                        .map(|parent| {
                            branch_map.get(parent.as_str()).copied().ok_or(
                                PythonArchiveError::Invalid("native parent identity missing"),
                            )
                        })
                        .transpose()?,
                    fork_sequence: branch.fork_sequence,
                    head_sequence: branch.head_sequence,
                    checkpoint: branch
                        .checkpoint_id
                        .as_ref()
                        .map(|checkpoint| {
                            id_map.get(checkpoint.as_str()).copied().ok_or(
                                PythonArchiveError::Invalid("native checkpoint identity missing"),
                            )
                        })
                        .transpose()?,
                },
                events,
                receipts: receipts
                    .remove(branch.branch_id.as_str())
                    .unwrap_or_default(),
            });
        }
        TimelineStore::validate_archive_receipts(&branches)
            .map_err(|_| PythonArchiveError::Invalid("native receipt replay rejected"))?;
        let provenance = self.provenance();
        TimelineStore::load_persisted_with_provenance(
            branches.clone(),
            vec![(session, provenance.clone())],
        )
        .map_err(|_| PythonArchiveError::Invalid("native session invariants rejected"))?;
        Ok(PreparedPythonCanonical {
            session,
            branches,
            provenance,
        })
    }
}

fn native_draft(
    event: &PythonEvent,
    identities: &HashMap<&str, EventId>,
    observers: &BTreeMap<String, CharacterId>,
    branches: &HashMap<&str, BranchId>,
    branch_info: &[super::PythonBranch],
) -> Result<EventDraft, PythonArchiveError> {
    let kind = match event.kind.as_str() {
        "PlayerMessage" => EventKind::PlayerMessage,
        "CharacterMessage" => EventKind::CharacterMessage,
        "CharacterObservation" => EventKind::CharacterObservation,
        "SessionNote" => EventKind::SessionNote,
        "Checkpoint" => EventKind::Checkpoint,
        "BranchCreated" => EventKind::BranchCreated,
        "UnityEvent" => EventKind::UnityEvent,
        "GameStarted" => EventKind::GameStarted,
        "GameAction" => EventKind::GameAction,
        "GameEnded" => EventKind::GameEnded,
        "PlayerInactive" => EventKind::PlayerInactive,
        "ProactiveOpportunity" => EventKind::ProactiveOpportunity,
        "CharacterJoined" => EventKind::CharacterJoined,
        "CharacterLeft" => EventKind::CharacterLeft,
        "CharacterContextReset" => EventKind::CharacterContextReset,
        _ => {
            return Err(PythonArchiveError::Invalid(
                "Python event semantics not representable in Rust",
            ));
        }
    };
    if contains_python_media(&event.payload) {
        return Err(PythonArchiveError::Invalid(
            "Python media requires typed native conversion",
        ));
    }
    if matches!(
        kind,
        EventKind::PlayerMessage
            | EventKind::CharacterMessage
            | EventKind::CharacterObservation
            | EventKind::SessionNote
    ) {
        let content = event
            .payload
            .get("content")
            .ok_or(PythonArchiveError::Invalid(
                "Python message content missing",
            ))?;
        let parts = crate::timeline::domain::parse_message_content(content).map_err(|_| {
            PythonArchiveError::Invalid("Python message content is not representable")
        })?;
        if parts.iter().any(|part| part.asset().is_some()) {
            return Err(PythonArchiveError::Invalid(
                "Python media requires typed native conversion",
            ));
        }
    }
    let actor = event
        .actor_id
        .as_ref()
        .map(|actor| match actor.as_str() {
            "Player" | "player" => Ok("Player".to_owned()),
            _ => observers
                .get(actor)
                .map(|id| id.as_str().to_owned())
                .ok_or(PythonArchiveError::Invalid("unbound Python actor")),
        })
        .transpose()?;
    let witnessed = event
        .observers
        .iter()
        .map(|id| {
            observers
                .get(id)
                .copied()
                .ok_or(PythonArchiveError::Invalid("unbound Python witness"))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let links = event
        .links
        .iter()
        .map(|(relation, target)| {
            Ok(CausalLink {
                relation: match relation.as_str() {
                    "reply_to" => CausalRelation::ReplyTo,
                    "react_to" => CausalRelation::ReactTo,
                    "caused_by" => CausalRelation::CausedBy,
                    _ => return Err(PythonArchiveError::Invalid("unsupported causal relation")),
                },
                event_id: *identities
                    .get(target.as_str())
                    .ok_or(PythonArchiveError::Invalid("native causal target missing"))?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut payload = event
        .payload
        .as_object()
        .cloned()
        .ok_or(PythonArchiveError::Invalid("event payload not an object"))?;
    if kind == EventKind::BranchCreated {
        let checkpoint = payload
            .get("checkpoint_id")
            .and_then(Value::as_str)
            .ok_or(PythonArchiveError::Invalid("fork checkpoint missing"))?;
        let checkpoint = identities
            .get(checkpoint)
            .ok_or(PythonArchiveError::Invalid(
                "native fork checkpoint missing",
            ))?;
        let parent = branch_info
            .iter()
            .find(|branch| branch.branch_id == event.branch_id)
            .and_then(|branch| branch.parent_branch_id.as_deref())
            .and_then(|id| branches.get(id))
            .ok_or(PythonArchiveError::Invalid("fork parent identity missing"))?;
        payload = json!({
            "parent_branch_id": parent.0.to_string(),
            "checkpoint_id": checkpoint.0.to_string(),
        })
        .as_object()
        .expect("known object")
        .clone();
    }
    let unix_microseconds = super::exact_time(&event.occurred_at)?.unix_microseconds;
    let occurred_at_unix_ms = u64::try_from(unix_microseconds.div_euclid(1000))
        .map_err(|_| PythonArchiveError::Invalid("native timestamp range"))?;
    let external_message_id = payload
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Ok(EventDraft {
        id: *identities
            .get(event.event_id.as_str())
            .ok_or(PythonArchiveError::Invalid("native event identity missing"))?,
        kind,
        payload,
        actor,
        observers: witnessed,
        links,
        external_message_id,
        occurred_at_unix_ms,
        schema_version: event.schema_version,
    })
}

fn contains_python_media(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("type").and_then(Value::as_str) == Some("image_ref")
                || map.values().any(contains_python_media)
        }
        Value::Array(values) => values.iter().any(contains_python_media),
        _ => false,
    }
}
