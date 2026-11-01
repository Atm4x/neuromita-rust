use uuid::Uuid;

use crate::identity::CharacterId;

use super::{EventKind, TimelineError, TimelineFact};

/// The narrow revision of accepted character replies is derived from immutable facts.
pub(crate) fn accepted_reply_count<'a>(
    facts: impl IntoIterator<Item = &'a TimelineFact>,
    character: CharacterId,
) -> Result<u64, TimelineError> {
    let actor = character.to_string();
    let mut count = 0_u64;
    for fact in facts {
        if fact.draft.kind == EventKind::CharacterMessage
            && fact.draft.actor.as_deref() == Some(actor.as_str())
            && fact
                .draft
                .payload
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| Uuid::parse_str(id).is_ok())
        {
            count = count
                .checked_add(1)
                .ok_or(TimelineError::Invalid("reply count overflow"))?;
        }
    }
    Ok(count)
}
