use serde_json::Value;
use uuid::Uuid;

use super::{BranchId, CausalRelation, EventDraft, EventKind, TimelineError, TimelineFact};
use crate::timeline::RegenerateAdmissionReceipt;

pub(crate) fn dispatch_may_have_started(
    facts: &[TimelineFact],
    request: Uuid,
) -> Result<Option<&TimelineFact>, TimelineError> {
    let mut evidence = None;
    for fact in facts {
        if fact.draft.kind == EventKind::RegenerateMayHaveDispatched
            && parse_dispatch(&fact.draft)? == request
        {
            if evidence.replace(fact).is_some() {
                return Err(TimelineError::Conflict);
            }
        }
    }
    Ok(evidence)
}

pub(crate) fn abandoned_regenerate(
    facts: &[TimelineFact],
    request: Uuid,
) -> Result<Option<&TimelineFact>, TimelineError> {
    let mut abandoned = None;
    for fact in facts {
        if fact.draft.kind == EventKind::RegenerateAbandoned
            && parse_abandoned(&fact.draft)? == request
        {
            if abandoned.replace(fact).is_some() {
                return Err(TimelineError::Conflict);
            }
        }
    }
    Ok(abandoned)
}

fn parse_dispatch(draft: &EventDraft) -> Result<Uuid, TimelineError> {
    parse_lifecycle_identity(draft)
}

fn parse_abandoned(draft: &EventDraft) -> Result<Uuid, TimelineError> {
    parse_lifecycle_identity(draft)
}

fn parse_lifecycle_identity(draft: &EventDraft) -> Result<Uuid, TimelineError> {
    if draft.payload.len() != 1
        || draft.actor.is_some()
        || !draft.observers.is_empty()
        || !draft.links.is_empty()
        || draft.external_message_id.is_some()
    {
        return Err(TimelineError::Invalid(
            "invalid regenerate abandonment fact",
        ));
    }
    draft
        .payload
        .get("request_id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .filter(|id| !id.is_nil())
        .ok_or(TimelineError::Invalid(
            "invalid regenerate abandonment identity",
        ))
}

/// One immutable rule for transient append, SQLite's locked append, and archive replay.
/// A terminal inherited from an ancestor still fences that request in descendants.
pub(crate) fn validate_regenerate_transition(
    branch: BranchId,
    facts: &[TimelineFact],
    drafts: &[EventDraft],
) -> Result<(), TimelineError> {
    let candidate_requests = drafts
        .iter()
        .filter_map(|draft| match draft.kind {
            EventKind::RegenerateAbandoned => Some(parse_abandoned(draft)),
            EventKind::RegenerateMayHaveDispatched => Some(parse_dispatch(draft)),
            EventKind::CharacterMessage => draft
                .payload
                .get("request_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .map(Ok),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    if candidate_requests.is_empty() {
        return Ok(());
    }

    for request in candidate_requests {
        let mut admission = None;
        for fact in facts
            .iter()
            .filter(|fact| fact.draft.kind == EventKind::BranchCreated)
        {
            if let Some(receipt) = RegenerateAdmissionReceipt::from_marker(&fact.draft.payload)?
                && receipt.request.0 == request
            {
                if admission.replace(receipt).is_some() {
                    return Err(TimelineError::Conflict);
                }
            }
        }
        let existing_dispatch = dispatch_may_have_started(facts, request)?.is_some();
        let mut dispatch_count = usize::from(existing_dispatch);
        let existing_abandoned = abandoned_regenerate(facts, request)?.is_some();
        let mut abandoned_count = usize::from(existing_abandoned);
        let mut accepted_count = facts
            .iter()
            .filter(|fact| {
                fact.draft.kind == EventKind::CharacterMessage
                    && fact
                        .draft
                        .payload
                        .get("request_id")
                        .and_then(Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())
                        == Some(request)
            })
            .count();
        for draft in drafts {
            match draft.kind {
                EventKind::RegenerateMayHaveDispatched if parse_dispatch(draft)? == request => {
                    let receipt = admission.as_ref().ok_or(TimelineError::Invalid(
                        "dispatch requires a durable regenerate admission",
                    ))?;
                    if receipt.child_branch != branch || abandoned_count > 0 || accepted_count > 0 {
                        return Err(TimelineError::Conflict);
                    }
                    dispatch_count += 1;
                }
                EventKind::RegenerateAbandoned if parse_abandoned(draft)? == request => {
                    let receipt = admission.as_ref().ok_or(TimelineError::Invalid(
                        "abandonment requires a durable regenerate admission",
                    ))?;
                    if receipt.child_branch != branch {
                        return Err(TimelineError::Invalid(
                            "abandonment requires admitted child",
                        ));
                    }
                    abandoned_count += 1;
                }
                EventKind::CharacterMessage
                    if draft
                        .payload
                        .get("request_id")
                        .and_then(Value::as_str)
                        .and_then(|id| Uuid::parse_str(id).ok())
                        == Some(request) =>
                {
                    if let Some(receipt) = admission.as_ref() {
                        let links = draft
                            .links
                            .iter()
                            .filter(|link| link.relation == CausalRelation::ReplyTo)
                            .collect::<Vec<_>>();
                        if branch != receipt.child_branch
                            || draft.actor.as_deref() != Some(receipt.character.as_str())
                            || !draft.observers.contains(&receipt.character)
                            || links.len() != 1
                            || links[0].event_id != receipt.original_input
                            || draft.payload.get("reply_to").and_then(Value::as_str)
                                != Some(receipt.original_input.0.to_string().as_str())
                        {
                            return Err(TimelineError::Invalid(
                                "regenerate acceptance violates durable admission",
                            ));
                        }
                        accepted_count += 1;
                    }
                }
                _ => {}
            }
        }
        if admission.is_some()
            && (dispatch_count > 1
                || abandoned_count > 1
                || accepted_count > 1
                || (abandoned_count > 0 && accepted_count > 0))
        {
            return Err(TimelineError::Conflict);
        }
    }
    Ok(())
}
