use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::{CharacterId, EventId, RequestId};

use super::domain::{BranchId, CausalRelation, EventKind, TimelineError, TimelineRepository};

/// An immutable, pending-dispatch admission recorded in the child fork marker.
/// It proves acceptance, NOT dispatch, completion or successful generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegenerateAdmissionReceipt {
    pub request: RequestId,
    pub accepted_request: RequestId,
    pub accepted_message: EventId,
    pub original_input: EventId,
    pub character: CharacterId,
    pub parent_branch: BranchId,
    pub parent_head: u64,
    pub parent_generation: u64,
    pub child_branch: BranchId,
    pub configuration: RegenerateConfigurationReference,
}

/// Stable identifiers captured at queue reservation. This is not a replayable
/// complete configuration snapshot; a later recovery worker must revalidate it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegenerateConfigurationReference {
    pub settings_revision: u64,
    pub preset_id: u32,
    pub provider_model: String,
    pub prompt_set: Option<String>,
}

impl RegenerateAdmissionReceipt {
    pub(crate) const KEY: &'static str = "regenerate_admission";

    pub(crate) fn from_marker(
        payload: &serde_json::Map<String, Value>,
    ) -> Result<Option<Self>, TimelineError> {
        payload
            .get(Self::KEY)
            .map(|raw| {
                serde_json::from_value(raw.clone())
                    .map_err(|_| TimelineError::Invalid("invalid durable regenerate receipt"))
            })
            .transpose()
    }

    pub(crate) fn validate(
        &self,
        store: &dyn TimelineRepository,
        parent: BranchId,
        child: BranchId,
        input: EventId,
        character: CharacterId,
    ) -> Result<(), TimelineError> {
        if self.parent_branch != parent
            || self.child_branch != child
            || self.original_input != input
            || self.character != character
            || self.parent_generation == 0
            || self.parent_head < 2
            || self.request.0.is_nil()
            || self.accepted_request.0.is_nil()
            || self.request == self.accepted_request
            || self.configuration.provider_model.trim().is_empty()
            || self
                .configuration
                .prompt_set
                .as_deref()
                .is_some_and(|set| set.trim().is_empty())
        {
            return Err(TimelineError::Invalid(
                "invalid regenerate admission identity",
            ));
        }
        let parent_info = store.branch(parent)?;
        if self.parent_head > parent_info.head_sequence {
            return Err(TimelineError::Invalid("regenerate parent head is missing"));
        }
        let input_fact = store.event(parent, input)?;
        let accepted = store
            .accepted_reply_at(
                parent,
                character,
                self.accepted_request.0,
                Some(self.parent_head),
            )?
            .filter(|fact| fact.draft.id == self.accepted_message)
            .ok_or(TimelineError::Invalid(
                "regenerate accepted reply is not canonical",
            ))?;
        let links = accepted
            .draft
            .links
            .iter()
            .filter(|link| link.relation == CausalRelation::ReplyTo)
            .collect::<Vec<_>>();
        if input_fact.draft.kind != EventKind::PlayerMessage
            || input_fact.draft.actor.as_deref() != Some("player")
            || !input_fact.draft.observers.contains(&character)
            || input_fact.sequence >= accepted.sequence
            || input_fact.sequence >= self.parent_head
            || links.len() != 1
            || links[0].event_id != input
            || accepted
                .draft
                .payload
                .get("reply_to")
                .and_then(Value::as_str)
                != Some(input.0.to_string().as_str())
        {
            return Err(TimelineError::Invalid("regenerate source/receipt mismatch"));
        }
        Ok(())
    }
}
