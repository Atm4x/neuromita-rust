use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use super::{BoundPythonArchive, PythonArchiveError, PythonEvent};
use crate::identity::{CharacterId, EventId};
use crate::timeline::domain::{
    Branch, BranchId, BranchInfo, CausalLink, CausalRelation, EventDraft, EventKind, Receipt,
    SessionId, SessionProvenance, TimelineAssetRef, TimelineError, TimelineFact,
    TimelineMessagePart, TimelineStore,
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
    /// Revalidate the current destination, stage verified binaries, and atomically
    /// install the complete canonical session with its exact Python provenance.
    /// If native publication fails, staged orphan blobs may remain, but no
    /// partial Timeline facts or receipts are exposed.
    pub fn import_memory(
        &self,
        destination: &mut TimelineStore,
        assets: &crate::timeline::storage::assets::TimelineAssetStore,
    ) -> Result<SessionId, PythonArchiveError> {
        let bound = self.source.bind_memory(destination, &self.observers)?;
        let prepared = bound.prepare_canonical()?;
        bound.stage_assets(assets)?;
        let session = prepared.session;
        destination
            .install_archive_branches_with_provenance(prepared.branches, Some(prepared.provenance))
            .map_err(native_install_error)?;
        Ok(session)
    }

    /// SQLite validates collisions again inside its write transaction. Asset
    /// staging precedes that transaction; a failed commit never publishes a
    /// partial session even when content-addressed orphan files remain.
    #[cfg(feature = "sqlite-system")]
    pub fn import_sqlite(
        &self,
        destination: &mut crate::timeline::storage::SqliteTimelineRepository,
        assets: &crate::timeline::storage::assets::TimelineAssetStore,
    ) -> Result<SessionId, PythonArchiveError> {
        let bound = self.source.bind_sqlite(destination, &self.observers)?;
        let prepared = bound.prepare_canonical()?;
        bound.stage_assets(assets)?;
        let session = prepared.session;
        destination
            .archive_install_with_provenance(prepared.branches, Some(prepared.provenance))
            .map_err(native_install_error)?;
        Ok(session)
    }

    /// Transform only known semantically equivalent Python events to native drafts
    /// and receipts. Verify the detached result with the *native* replay validator.
    /// Python `EventObserved` remains unsupported: disclosure of an old world
    /// event cannot be approximated as a native private observation.
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
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let asset_catalog = self
            .source
            .envelope
            .assets
            .iter()
            .map(|asset| {
                let size = STANDARD
                    .decode(&asset.base64)
                    .map_err(|_| PythonArchiveError::Invalid("verified asset encoding changed"))?
                    .len();
                Ok((asset.asset_id.as_str(), (asset.media_type.as_str(), size)))
            })
            .collect::<Result<HashMap<_, _>, PythonArchiveError>>()?;
        let mut drafts = HashMap::new();
        for event in &self.source.envelope.events {
            let draft = native_draft(
                event,
                &id_map,
                &self.observers,
                &branch_map,
                &self.source.envelope.branches,
                &event_lookup,
                &asset_catalog,
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

/// Convert Python's only verified media layout into the canonical typed content.
/// The same operation serves original messages and presentation revisions.
fn convert_message_content(
    payload: &mut serde_json::Map<String, Value>,
    assets: &HashMap<&str, (&str, usize)>,
) -> Result<(), PythonArchiveError> {
    let attachments = payload.remove("attachments");
    if contains_python_media(&Value::Object(payload.clone())) {
        return Err(PythonArchiveError::Invalid(
            "Python media requires typed native conversion",
        ));
    }
    let Some(attachments) = attachments else {
        let Some(content) = payload.get("content") else {
            return Ok(());
        };
        let parts = crate::timeline::domain::parse_message_content(content).map_err(|_| {
            PythonArchiveError::Invalid("Python message content is not representable")
        })?;
        if parts.iter().any(|part| part.asset().is_some()) {
            return Err(PythonArchiveError::Invalid(
                "Python media requires typed native conversion",
            ));
        }
        return Ok(());
    };
    let list = attachments
        .as_array()
        .filter(|list| !list.is_empty() && list.len() <= 31)
        .ok_or(PythonArchiveError::Invalid("Python attachment count"))?;
    let text =
        payload
            .get("content")
            .and_then(Value::as_str)
            .ok_or(PythonArchiveError::Invalid(
                "Python attachment text missing",
            ))?;
    let mut parts = vec![TimelineMessagePart::Text {
        text: text.to_owned(),
    }];
    for entry in list {
        let object = entry
            .as_object()
            .filter(|value| value.len() == 4)
            .ok_or(PythonArchiveError::Invalid("Python attachment shape"))?;
        if object.get("type").and_then(Value::as_str) != Some("image_ref") {
            return Err(PythonArchiveError::Invalid(
                "unsupported Python attachment type",
            ));
        }
        let external = object
            .get("asset_id")
            .and_then(Value::as_str)
            .ok_or(PythonArchiveError::Invalid("Python attachment id"))?;
        let digest = external
            .strip_prefix("sha256:")
            .ok_or(PythonArchiveError::Invalid("Python attachment id"))?;
        let mime = object
            .get("media_type")
            .and_then(Value::as_str)
            .ok_or(PythonArchiveError::Invalid("Python attachment MIME"))?;
        let size = object
            .get("byte_size")
            .and_then(Value::as_u64)
            .ok_or(PythonArchiveError::Invalid("Python attachment size"))?;
        if !mime.starts_with("image/")
            || assets
                .get(external)
                .map(|(catalog_mime, len)| *catalog_mime != mime || *len as u64 != size)
                .unwrap_or(true)
        {
            return Err(PythonArchiveError::Invalid("Python attachment mismatch"));
        }
        parts.push(TimelineMessagePart::Image {
            asset: TimelineAssetRef {
                digest: digest.to_owned(),
                size,
                media_type: mime.to_owned(),
            },
        });
    }
    payload.insert(
        "content".into(),
        serde_json::to_value(parts).map_err(|_| PythonArchiveError::Encoding)?,
    );
    Ok(())
}

fn native_install_error(error: TimelineError) -> PythonArchiveError {
    match error {
        TimelineError::Conflict => PythonArchiveError::Invalid("native session identity conflict"),
        _ => PythonArchiveError::Invalid("native session installation failed"),
    }
}

fn native_draft(
    event: &PythonEvent,
    identities: &HashMap<&str, EventId>,
    observers: &BTreeMap<String, CharacterId>,
    branches: &HashMap<&str, BranchId>,
    branch_info: &[super::PythonBranch],
    events: &HashMap<&str, &PythonEvent>,
    assets: &HashMap<&str, (&str, usize)>,
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
        "MessageHidden" => EventKind::MessageHidden,
        "MessageRevised" => EventKind::MessageRevised,
        "MessageRestored" => EventKind::MessageRestored,
        _ => {
            return Err(PythonArchiveError::Invalid(
                "Python event semantics not representable in Rust",
            ));
        }
    };
    let mut payload = event
        .payload
        .as_object()
        .cloned()
        .ok_or(PythonArchiveError::Invalid("event payload not an object"))?;
    if matches!(
        kind,
        EventKind::PlayerMessage | EventKind::CharacterMessage | EventKind::CharacterObservation
    ) || (kind == EventKind::SessionNote
        && (payload.contains_key("content") || payload.contains_key("attachments")))
    {
        convert_message_content(&mut payload, assets)?;
    } else if kind == EventKind::SessionNote {
        if contains_python_media(&Value::Object(payload.clone())) {
            return Err(PythonArchiveError::Invalid(
                "Python media requires typed native conversion",
            ));
        }
    } else if kind == EventKind::MessageHidden {
        if payload.len() != 1 {
            return Err(PythonArchiveError::Invalid(
                "unsupported Python hide payload",
            ));
        }
        let source_ids = payload
            .get("event_ids")
            .and_then(Value::as_array)
            .ok_or(PythonArchiveError::Invalid("Python hide targets missing"))?;
        let targets = source_ids
            .iter()
            .map(|id| {
                let source = id
                    .as_str()
                    .ok_or(PythonArchiveError::Invalid("Python hide target invalid"))?;
                identities
                    .get(source)
                    .map(|id| id.0.to_string())
                    .ok_or(PythonArchiveError::Invalid("Python hide target missing"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        payload.insert("event_ids".into(), json!(targets));
    } else if matches!(kind, EventKind::MessageRevised | EventKind::MessageRestored) {
        let required_fields = if kind == EventKind::MessageRevised {
            2
        } else {
            1
        };
        if payload.len() != required_fields {
            return Err(PythonArchiveError::Invalid(
                "unsupported Python presentation payload",
            ));
        }
        let source =
            payload
                .get("event_id")
                .and_then(Value::as_str)
                .ok_or(PythonArchiveError::Invalid(
                    "Python presentation target missing",
                ))?;
        let target = identities.get(source).ok_or(PythonArchiveError::Invalid(
            "Python presentation target unknown",
        ))?;
        if kind == EventKind::MessageRevised {
            let original = events.get(source).ok_or(PythonArchiveError::Invalid(
                "Python revision original missing",
            ))?;
            let changes = payload
                .get_mut("message")
                .and_then(Value::as_object_mut)
                .ok_or(PythonArchiveError::Invalid(
                    "Python revision changes missing",
                ))?;
            if let Some(external_id) = changes.get("message_id") {
                if original.payload.get("message_id") != Some(external_id) {
                    return Err(PythonArchiveError::Invalid(
                        "Python revision identity changed",
                    ));
                }
                changes.remove("message_id");
            }
            convert_message_content(changes, assets)?;
        }
        payload.insert("event_id".into(), json!(target.0.to_string()));
    } else if contains_python_media(&Value::Object(payload.clone())) {
        return Err(PythonArchiveError::Invalid(
            "Python media requires typed native conversion",
        ));
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
