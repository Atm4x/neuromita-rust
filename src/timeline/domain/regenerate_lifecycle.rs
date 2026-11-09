use serde_json::Value;
use uuid::Uuid;

use super::{BranchId, CausalRelation, EventDraft, EventKind, TimelineError, TimelineFact};
use crate::timeline::RegenerateAdmissionReceipt;
use crate::timeline::regenerate_receipt::SameBranchRegenerateReceipt;
use std::collections::HashSet;

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
pub(crate) fn validate_same_branch_admission(
    branch: BranchId,
    facts: &[TimelineFact],
    drafts: &[EventDraft],
) -> Result<(), TimelineError> {
    let markers: Vec<_> = drafts
        .iter()
        .enumerate()
        .filter(|(_, draft)| draft.kind == EventKind::RegenerateAdmitted)
        .collect();
    if markers.is_empty() {
        return Ok(());
    }
    if markers.len() != 1 || markers[0].0 != drafts.len() - 1 {
        return Err(TimelineError::Invalid("same-branch admission must be last"));
    }
    let receipt = SameBranchRegenerateReceipt::from_fact(markers[0].1)?;
    if receipt.branch != branch
        || receipt.source_head != facts.last().map_or(0, |fact| fact.sequence)
        || receipt.selection_generation == 0
        || receipt.request.0.is_nil()
        || receipt.accepted_request.0.is_nil()
        || receipt.request == receipt.accepted_request
        || receipt.hidden_messages.is_empty()
        || receipt.configuration.provider_model.trim().is_empty()
        || receipt
            .configuration
            .prompt_set
            .as_deref()
            .is_some_and(|s| s.trim().is_empty())
        || receipt.forget_operation.is_some() != !receipt.forgotten_events.is_empty()
    {
        return Err(TimelineError::Invalid(
            "invalid same-branch admission identity",
        ));
    }
    if facts.iter().any(|fact| {
        fact.draft.kind == EventKind::RegenerateAdmitted
            && SameBranchRegenerateReceipt::from_fact(&fact.draft)
                .is_ok_and(|old| old.request == receipt.request)
    }) {
        return Err(TimelineError::Conflict);
    }
    let accepted = facts
        .iter()
        .find(|fact| {
            fact.draft.id == receipt.accepted_message
                && fact.draft.kind == EventKind::CharacterMessage
                && fact.draft.actor.as_deref() == Some(receipt.character.as_str())
                && fact.draft.payload.get("request_id").and_then(Value::as_str)
                    == Some(receipt.accepted_request.0.to_string().as_str())
        })
        .ok_or(TimelineError::Invalid("missing canonical accepted reply"))?;
    let input = facts
        .iter()
        .find(|fact| {
            fact.draft.id == receipt.original_input
                && fact.draft.kind == EventKind::PlayerMessage
                && fact.draft.actor.as_deref() == Some("player")
                && fact.draft.observers.contains(&receipt.character)
        })
        .ok_or(TimelineError::Invalid("missing canonical original input"))?;
    if input.sequence >= accepted.sequence
        || accepted
            .draft
            .links
            .iter()
            .filter(|link| link.relation == CausalRelation::ReplyTo)
            .map(|link| link.event_id)
            .collect::<Vec<_>>()
            != vec![receipt.original_input]
        || accepted
            .draft
            .payload
            .get("reply_to")
            .and_then(Value::as_str)
            != Some(receipt.original_input.0.to_string().as_str())
    {
        return Err(TimelineError::Invalid("admission causal source mismatch"));
    }
    // A valid marker must represent exactly what the current, historically
    // revisioned character projection would replace. No arbitrary witness's
    // message can be silently hidden by a forged direct append.
    let projected = crate::timeline::view::MitaView::from_facts(
        branch,
        receipt.source_head,
        receipt.character,
        facts,
    );
    let visible = projected.messages();
    if !visible
        .iter()
        .any(|message| message.event_id == receipt.original_input)
        || !visible
            .iter()
            .any(|message| message.event_id == receipt.accepted_message)
    {
        return Err(TimelineError::Invalid(
            "admission source is no longer visible",
        ));
    }
    let expected_hidden = visible
        .iter()
        .filter(|message| {
            message.sequence >= accepted.sequence
                && message.kind == EventKind::CharacterMessage
                && message.speaker.as_deref() == Some(receipt.character.as_str())
        })
        .map(|message| message.event_id)
        .collect::<Vec<_>>();
    if receipt.hidden_messages != expected_hidden {
        return Err(TimelineError::Invalid(
            "admission must replace exactly the character replies",
        ));
    }
    let hidden = expected_hidden.iter().copied().collect::<HashSet<_>>();
    let known_ids = crate::timeline::domain::character_view(facts, receipt.character)
        .into_iter()
        .map(|known| known.fact.draft.id)
        .collect::<HashSet<_>>();
    let expected_forgotten = facts
        .iter()
        .filter(|fact| hidden.contains(&fact.draft.id) && known_ids.contains(&fact.draft.id))
        .map(|fact| fact.draft.id)
        .collect::<Vec<_>>();
    if receipt.forgotten_events != expected_forgotten {
        return Err(TimelineError::Invalid("admission forget targets mismatch"));
    }
    if facts.iter().any(|fact| {
        fact.sequence >= accepted.sequence
            && fact.draft.actor.as_deref() == Some(receipt.character.as_str())
            && matches!(
                fact.draft.kind,
                EventKind::CharacterStateChanged
                    | EventKind::CharacterWorkingStateReplaced
                    | EventKind::CharacterVariablesChanged
                    | EventKind::CharacterMemoryChanged
                    | EventKind::GameAction
                    | EventKind::GameStateCaptured
            )
    }) {
        return Err(TimelineError::Invalid(
            "admission cannot undo stateful effects",
        ));
    }
    let hide = &drafts[0];
    if hide.kind != EventKind::MessageHidden
        || hide.id != receipt.hide_operation
        || hide.observers.iter().copied().collect::<Vec<_>>() != vec![receipt.character]
        || hide.payload.get("event_ids")
            != Some(&serde_json::json!(
                receipt
                    .hidden_messages
                    .iter()
                    .map(|id| id.0.to_string())
                    .collect::<Vec<_>>()
            ))
        || !receipt.hidden_messages.contains(&receipt.accepted_message)
        || receipt
            .hidden_messages
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            != receipt.hidden_messages.len()
        || receipt
            .forgotten_events
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            != receipt.forgotten_events.len()
    {
        return Err(TimelineError::Invalid("admission hide targets mismatch"));
    }
    let expected_count = if receipt.forget_operation.is_some() {
        3
    } else {
        2
    };
    if drafts.len() != expected_count {
        return Err(TimelineError::Invalid(
            "admission must atomically include controls",
        ));
    }
    if let Some(forget_id) = receipt.forget_operation {
        let forget = &drafts[1];
        if forget.kind != EventKind::CharacterKnowledgeForgotten
            || forget.id != forget_id
            || forget.observers.iter().copied().collect::<Vec<_>>() != vec![receipt.character]
            || forget.payload.get("event_ids")
                != Some(&serde_json::json!(
                    receipt
                        .forgotten_events
                        .iter()
                        .map(|id| id.0.to_string())
                        .collect::<Vec<_>>()
                ))
        {
            return Err(TimelineError::Invalid("admission forget targets mismatch"));
        }
    }
    Ok(())
}

pub(crate) fn validate_regenerate_transition(
    branch: BranchId,
    facts: &[TimelineFact],
    drafts: &[EventDraft],
) -> Result<(), TimelineError> {
    validate_same_branch_admission(branch, facts, drafts)?;
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
        // Legacy fork receipts and same-branch admissions are distinct, explicit
        // durable protocols. Never infer one from the absence of the other.
        let mut legacy = None;
        let mut same_branch = None;
        for fact in facts {
            match fact.draft.kind {
                EventKind::BranchCreated => {
                    if let Some(receipt) =
                        RegenerateAdmissionReceipt::from_marker(&fact.draft.payload)?
                        && receipt.request.0 == request
                    {
                        if legacy.replace(receipt).is_some() {
                            return Err(TimelineError::Conflict);
                        }
                    }
                }
                EventKind::RegenerateAdmitted => {
                    let receipt = SameBranchRegenerateReceipt::from_fact(&fact.draft)?;
                    if receipt.request.0 == request {
                        if same_branch.replace(receipt).is_some() {
                            return Err(TimelineError::Conflict);
                        }
                    }
                }
                _ => {}
            }
        }
        if legacy.is_some() && same_branch.is_some() {
            return Err(TimelineError::Conflict);
        }
        let identity = legacy
            .as_ref()
            .map(|receipt| {
                (
                    receipt.child_branch,
                    receipt.character,
                    receipt.original_input,
                )
            })
            .or_else(|| {
                same_branch
                    .as_ref()
                    .map(|receipt| (receipt.branch, receipt.character, receipt.original_input))
            });
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
                    let (admitted_branch, _, _) = identity.ok_or(TimelineError::Invalid(
                        "dispatch requires a durable regenerate admission",
                    ))?;
                    if admitted_branch != branch || abandoned_count > 0 || accepted_count > 0 {
                        return Err(TimelineError::Conflict);
                    }
                    dispatch_count += 1;
                }
                EventKind::RegenerateAbandoned if parse_abandoned(draft)? == request => {
                    let (admitted_branch, _, _) = identity.ok_or(TimelineError::Invalid(
                        "abandonment requires a durable regenerate admission",
                    ))?;
                    if admitted_branch != branch {
                        return Err(TimelineError::Invalid(
                            "abandonment requires admitted branch",
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
                    if let Some((admitted_branch, character, input)) = identity {
                        let links = draft
                            .links
                            .iter()
                            .filter(|link| link.relation == CausalRelation::ReplyTo)
                            .collect::<Vec<_>>();
                        if let Some(receipt) = same_branch.as_ref() {
                            let marker_sequence = facts
                                .iter()
                                .find(|fact| {
                                    fact.branch_id == branch
                                        && fact.draft.kind == EventKind::RegenerateAdmitted
                                        && fact.draft.id != draft.id
                                        && SameBranchRegenerateReceipt::from_fact(&fact.draft)
                                            .is_ok_and(|existing| {
                                                existing.request == receipt.request
                                            })
                                })
                                .map(|fact| fact.sequence)
                                .ok_or(TimelineError::NotFound)?;
                            if dispatch_count == 0 {
                                return Err(TimelineError::Invalid(
                                    "same-branch acceptance requires pre-provider dispatch",
                                ));
                            }
                            if facts.iter().any(|fact| {
                                fact.sequence > marker_sequence
                                    && fact.draft.observers.contains(&character)
                                    && matches!(
                                        fact.draft.kind,
                                        EventKind::CharacterContextReset
                                            | EventKind::CharacterKnowledgeForgotten
                                            | EventKind::CharacterKnowledgeRevealed
                                    )
                            }) {
                                return Err(TimelineError::ConcurrentWrite);
                            }
                        }
                        if branch != admitted_branch
                            || draft.actor.as_deref() != Some(character.as_str())
                            || !draft.observers.contains(&character)
                            || links.len() != 1
                            || links[0].event_id != input
                            || draft.payload.get("reply_to").and_then(Value::as_str)
                                != Some(input.0.to_string().as_str())
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
        if identity.is_some()
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
