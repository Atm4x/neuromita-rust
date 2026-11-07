use super::*;
use chrono::{DateTime, FixedOffset};
use ring::digest::{SHA256, digest};

pub(super) fn checked_id(value: &str) -> Result<(), PythonArchiveError> {
    if value.is_empty() || value.trim() != value || value.len() > 16 * 1024 {
        return Err(PythonArchiveError::Invalid("external identity"));
    }
    Ok(())
}

pub(super) fn exact_time(value: &str) -> Result<ExactPythonTimestamp, PythonArchiveError> {
    let date = DateTime::<FixedOffset>::parse_from_rfc3339(value)
        .map_err(|_| PythonArchiveError::Invalid("timestamp must be ISO 8601 with timezone"))?;
    Ok(ExactPythonTimestamp {
        source_iso8601: value.to_owned(),
        unix_microseconds: date.timestamp_micros(),
    })
}

pub(super) fn mapped(kind: &str, session: &str, external: &str) -> ExternalIdentity {
    let mut input = Vec::with_capacity(kind.len() + session.len() + external.len() + 40);
    input.extend_from_slice(b"neuromita-python-timeline-id\0");
    input.extend_from_slice(kind.as_bytes());
    input.push(0);
    input.extend_from_slice(session.as_bytes());
    input.push(0);
    input.extend_from_slice(external.as_bytes());
    let sha = digest(&SHA256, &input);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&sha.as_ref()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    ExternalIdentity {
        source: external.to_owned(),
        candidate_uuid: Uuid::from_bytes(bytes),
    }
}

pub(super) fn canonical_intent(event: &PythonEvent) -> Value {
    let mut observers = event.observers.clone();
    observers.sort();
    let mut links = event.links.clone();
    links.sort();
    json!({
        "event_id": event.event_id,
        "kind": event.kind,
        "payload": event.payload,
        "actor_id": event.actor_id,
        "observers": observers,
        "schema_version": event.schema_version,
        "links": links,
    })
}

pub(super) fn known_python_kind(kind: &str) -> bool {
    matches!(
        kind,
        "PlayerMessage"
            | "CharacterMessage"
            | "CharacterObservation"
            | "CharacterJoined"
            | "CharacterLeft"
            | "UnityEvent"
            | "GameStarted"
            | "GameAction"
            | "GameEnded"
            | "PlayerInactive"
            | "ProactiveOpportunity"
            | "EventObserved"
            | "SessionNote"
            | "MessageHidden"
            | "MessageRevised"
            | "MessageRestored"
            | "CharacterContextReset"
            | "Checkpoint"
            | "BranchCreated"
    )
}

pub(super) fn is_history(kind: &str) -> bool {
    matches!(
        kind,
        "PlayerMessage" | "CharacterMessage" | "CharacterObservation" | "SessionNote"
    )
}

pub(super) fn observe_python<'a>(
    event: &'a PythonEvent,
    knowledge: &mut HashMap<&'a str, HashSet<&'a str>>,
) {
    for observer in &event.observers {
        let seen = knowledge.entry(observer).or_default();
        if event.kind == "CharacterContextReset" {
            seen.clear();
        }
        seen.insert(&event.event_id);
        if event.kind == "EventObserved" {
            for (relation, target) in &event.links {
                if relation == "observes" {
                    seen.insert(target);
                }
            }
        }
    }
}

pub(super) fn validate_python_event(
    event: &PythonEvent,
    prior: &HashMap<&str, &PythonEvent>,
    knowledge: &HashMap<&str, HashSet<&str>>,
) -> Result<(), PythonArchiveError> {
    let mut counts = HashMap::<&str, usize>::new();
    for (relation, target) in &event.links {
        let target_event = prior
            .get(target.as_str())
            .ok_or(PythonArchiveError::Invalid(
                "causal target not in preceding world",
            ))?;
        *counts.entry(relation.as_str()).or_default() += 1;
        if relation == "reply_to"
            && !matches!(
                target_event.kind.as_str(),
                "PlayerMessage" | "CharacterMessage"
            )
        {
            return Err(PythonArchiveError::Invalid(
                "reply target must be a message",
            ));
        }
        if matches!(relation.as_str(), "reply_to" | "react_to")
            && (event.kind != "CharacterMessage"
                || !event
                    .actor_id
                    .as_deref()
                    .and_then(|actor| knowledge.get(actor))
                    .is_some_and(|seen| seen.contains(target.as_str())))
        {
            return Err(PythonArchiveError::Invalid(
                "reply target is not visible to actor",
            ));
        }
    }
    if ["reply_to", "react_to", "observes"]
        .iter()
        .any(|rel| counts.get(rel).copied().unwrap_or(0) > 1)
        || (counts.contains_key("reply_to") && counts.contains_key("react_to"))
    {
        return Err(PythonArchiveError::Invalid("ambiguous causal relation"));
    }
    if event.kind == "EventObserved" {
        if event.observers.is_empty() || counts.get("observes") != Some(&1) {
            return Err(PythonArchiveError::Invalid(
                "invalid observation disclosure",
            ));
        }
    } else if counts.contains_key("observes") {
        return Err(PythonArchiveError::Invalid(
            "unexpected observation disclosure",
        ));
    }
    if matches!(event.kind.as_str(), "CharacterJoined" | "CharacterLeft")
        && !event
            .actor_id
            .as_ref()
            .is_some_and(|actor| event.observers.contains(actor))
    {
        return Err(PythonArchiveError::Invalid(
            "presence requires actor witness",
        ));
    }
    if event.kind == "Checkpoint" && !event.payload.get("snapshot").is_some_and(Value::is_object) {
        return Err(PythonArchiveError::Invalid("checkpoint snapshot"));
    }
    if matches!(
        event.kind.as_str(),
        "MessageHidden" | "MessageRevised" | "MessageRestored"
    ) {
        let target_ids = if event.kind == "MessageHidden" {
            event
                .payload
                .get("event_ids")
                .and_then(Value::as_array)
                .cloned()
                .ok_or(PythonArchiveError::Invalid("missing message targets"))?
        } else {
            vec![
                event
                    .payload
                    .get("event_id")
                    .cloned()
                    .unwrap_or(Value::Null),
            ]
        };
        if event.observers.is_empty() || target_ids.is_empty() {
            return Err(PythonArchiveError::Invalid(
                "message change needs observers",
            ));
        }
        for target in &target_ids {
            let id = target
                .as_str()
                .ok_or(PythonArchiveError::Invalid("invalid message target"))?;
            let message = prior
                .get(id)
                .ok_or(PythonArchiveError::Invalid("unknown message target"))?;
            if !is_history(&message.kind)
                || event.observers.iter().any(|observer| {
                    !knowledge
                        .get(observer.as_str())
                        .is_some_and(|seen| seen.contains(id))
                })
            {
                return Err(PythonArchiveError::Invalid("message target is not visible"));
            }
            if event.kind == "MessageRevised" {
                let replacement = event
                    .payload
                    .get("message")
                    .and_then(Value::as_object)
                    .ok_or(PythonArchiveError::Invalid("missing revision body"))?;
                if !replacement.contains_key("content")
                    || [
                        "role",
                        "speaker",
                        "sender",
                        "timeline_event_id",
                        "timeline_sequence",
                        "timeline_learned_at",
                        "_history_row_id",
                        "time",
                    ]
                    .iter()
                    .any(|key| replacement.contains_key(*key))
                {
                    return Err(PythonArchiveError::Invalid(
                        "revision changes protected identity",
                    ));
                }
                if replacement
                    .get("message_id")
                    .is_some_and(|value| Some(value) != message.payload.get("message_id"))
                {
                    return Err(PythonArchiveError::Invalid(
                        "revision changes external message id",
                    ));
                }
            }
        }
    }
    if event.kind == "CharacterContextReset" && event.observers.is_empty() {
        return Err(PythonArchiveError::Invalid("context reset needs observers"));
    }
    Ok(())
}
