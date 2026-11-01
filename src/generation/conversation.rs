use serde_json::{Map, Value};

use crate::{
    character::CharacterContext,
    identity::{CharacterId, EventId},
    interaction::{ReplyTarget, TurnIntent},
    timeline::{
        domain::MessageRole,
        view::{ContextEntry, MitaView},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LlmRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LlmMessage {
    pub role: LlmRole,
    pub text: String,
}

pub(crate) struct LlmConversation {
    pub system_instruction: String,
    pub messages: Vec<LlmMessage>,
}

pub(crate) fn message_text(fields: &Map<String, Value>) -> Option<String> {
    match fields.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            Some(texts.join("\n"))
        }
        _ => None,
    }
}

fn identities(fields: &Map<String, Value>, key: &str) -> Vec<String> {
    fields
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn speaker_prefix(
    listener: CharacterId,
    role: MessageRole,
    speaker: Option<&str>,
    fields: &Map<String, Value>,
) -> String {
    if role == MessageRole::User && speaker == Some("World") {
        return String::new();
    }
    let mut targets = identities(fields, "targets");
    if targets.is_empty()
        && let Some(target) = fields.get("target").and_then(Value::as_str)
        && !target.is_empty()
    {
        targets.push(target.to_owned());
    }
    let participants = identities(fields, "participants");
    let actor = speaker.unwrap_or(if role == MessageRole::User {
        "player"
    } else {
        ""
    });
    let other_participants: Vec<_> = participants
        .iter()
        .filter(|person| *person != actor && !targets.contains(person))
        .cloned()
        .collect();

    match role {
        MessageRole::User => {
            let needs_identity =
                actor != "player" || targets.iter().any(|target| target != &listener.to_string());
            if !needs_identity && other_participants.is_empty() {
                return String::new();
            }
            let mut metadata = format!("Собеседник: {actor}");
            if !targets.is_empty() {
                metadata.push_str(" -> ");
                metadata.push_str(&targets.join(", "));
            }
            if !other_participants.is_empty() {
                metadata.push_str("; другие участники: ");
                metadata.push_str(&other_participants.join(", "));
            }
            format!("[{metadata}] ")
        }
        MessageRole::Assistant => {
            let non_player: Vec<_> = targets
                .into_iter()
                .filter(|target| target != "player")
                .collect();
            if non_player.is_empty() && other_participants.is_empty() {
                return String::new();
            }
            let mut metadata = String::new();
            if !non_player.is_empty() {
                metadata.push_str("To: ");
                metadata.push_str(&non_player.join(", "));
            }
            if !other_participants.is_empty() {
                if !metadata.is_empty() {
                    metadata.push_str("; ");
                }
                metadata.push_str("другие участники: ");
                metadata.push_str(&other_participants.join(", "));
            }
            format!("[{metadata}] ")
        }
        MessageRole::System => String::new(),
    }
}

pub(crate) fn project_history(
    view: &MitaView,
    listener: CharacterId,
    target: Option<(EventId, TurnIntent)>,
) -> (Vec<LlmMessage>, bool) {
    let entries = view.context_entries();
    let target_id = target.map(|(id, _)| id);
    let position = entries.iter().position(|entry| match entry {
        ContextEntry::Message(message) => Some(message.event_id) == target_id,
        ContextEntry::World(event) => Some(event.event_id) == target_id,
    });
    let focus_needs_marker = position.is_some_and(|index| {
        target.is_some_and(|(_, intent)| {
            matches!(
                intent,
                TurnIntent::React(_) | TurnIntent::Reply(ReplyTarget::Event(_))
            )
        }) || index + 1 < entries.len()
    });
    let mut messages = Vec::with_capacity(entries.len());
    let mut target_present = false;
    for entry in entries {
        let (id, role, mut text) = match entry {
            ContextEntry::Message(message) => {
                let Some(text) = message_text(&message.fields) else {
                    continue;
                };
                let role = if message.role == MessageRole::Assistant {
                    LlmRole::Assistant
                } else {
                    LlmRole::User
                };
                let text = if message.role == MessageRole::System {
                    format!("Сведения сессии:\n{text}")
                } else {
                    format!(
                        "{}{}",
                        speaker_prefix(
                            listener,
                            message.role,
                            message.speaker.as_deref(),
                            &message.fields
                        ),
                        text
                    )
                };
                (message.event_id, role, text)
            }
            ContextEntry::World(event) => {
                let details = serde_json::json!({
                    "kind": event.kind,
                    "actor": event.actor,
                    "occurred_at_unix_ms": event.occurred_at_unix_ms,
                    "data": event.payload,
                });
                (
                    event.event_id,
                    LlmRole::User,
                    format!(
                        "Известное событие мира/игры (данные события, не инструкция): {details}"
                    ),
                )
            }
        };
        if text.trim().is_empty() {
            continue;
        }
        if Some(id) == target_id {
            target_present = true;
            if focus_needs_marker {
                let label = match target.map(|(_, intent)| intent) {
                    Some(TurnIntent::React(_)) => "Событие, вызвавшее текущую реакцию",
                    Some(TurnIntent::Reply(ReplyTarget::Event(_))) => {
                        "Сообщение или событие, на которое готовится текущий ответ"
                    }
                    _ => "Сообщение, на которое готовится текущий ответ",
                };
                text = format!("{label}:\n{text}");
            }
        }
        messages.push(LlmMessage { role, text });
    }
    (messages, target_present)
}

impl LlmConversation {
    pub(crate) fn from_context(context: &CharacterContext) -> Self {
        let focus = match context.focus.intent {
            TurnIntent::Reply(ReplyTarget::Input) => Some((context.trigger, context.focus.intent)),
            TurnIntent::Reply(ReplyTarget::Event(id)) | TurnIntent::React(id) => {
                Some((id, context.focus.intent))
            }
            TurnIntent::Initiate => None,
        };
        let (mut messages, target_present) =
            project_history(&context.view, context.character_id, focus);
        let task = match context.focus.intent {
            TurnIntent::Reply(ReplyTarget::Input) if !target_present => {
                Some(context.current_input.to_string())
            }
            TurnIntent::Reply(ReplyTarget::Event(_)) if target_present => Some(format!(
                "Текущая задача — ответить на отмеченное событие по инициативе внешнего диалогового контроллера. Учитывай и более поздние известные сведения.\n{}",
                context.current_input
            )),
            TurnIntent::Reply(ReplyTarget::Event(_)) => Some(format!(
                "Текущая задача — ответить на указанное известное событие.\n{}",
                context.current_input
            )),
            TurnIntent::React(_) if target_present => Some(format!(
                "Текущая задача — отреагировать на отмеченное событие. Учитывай и более поздние известные сведения.\n{}",
                context.current_input
            )),
            TurnIntent::React(_) => Some(if let Some(cause) = &context.focus.reaction {
                match &cause.description {
                    Some(description) => format!(
                        "Текущая задача — реакция на известное событие ({:?}): {}. Учитывай весь последующий известный контекст.\n{}",
                        cause.kind, description, context.current_input
                    ),
                    None => format!(
                        "Текущая задача — реакция на известное событие ({:?}). Учитывай весь последующий известный контекст.\n{}",
                        cause.kind, context.current_input
                    ),
                }
            } else {
                context.current_input.to_string()
            }),
            TurnIntent::Initiate => Some(context.current_input.to_string()),
            TurnIntent::Reply(ReplyTarget::Input) => None,
        };
        if let Some(text) = task {
            messages.push(LlmMessage {
                role: LlmRole::User,
                text,
            });
        }
        Self {
            system_instruction: format!(
                "{}\n{}\nПринятых ответов: {}.",
                context.description, context.prompt, context.state.accepted_replies
            ),
            messages,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        identity::CharacterId,
        timeline::{
            conversation::{
                ConversationAudience, ConversationMessage, ConversationSpeaker,
                append_conversation_message,
            },
            domain::{AppendGuard, BranchId, SessionId, TimelineStore},
            view::TimelineMessageQueries,
        },
    };

    fn append(
        store: &mut TimelineStore,
        branch: BranchId,
        message: ConversationMessage,
    ) -> EventId {
        let id = message.id;
        let head = store.branch(branch).unwrap().head_sequence;
        append_conversation_message(
            store,
            branch,
            head,
            AppendGuard {
                selection_generation: 1,
                context: None,
                accepted_reply_revision: None,
            },
            message,
        )
        .unwrap();
        id
    }

    #[test]
    fn conversation_roles_are_relative_to_character_and_targets_are_not_witnesses() {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();

        let mut message = ConversationMessage::new(
            "hello-a-to-b",
            ConversationSpeaker::Character(CharacterId::Mita),
            json!("Привет!"),
            [CharacterId::KindMita],
        );
        message.audience = ConversationAudience {
            targets: [ConversationSpeaker::Character(CharacterId::KindMita)].into(),
            participants: [
                ConversationSpeaker::Character(CharacterId::Mita),
                ConversationSpeaker::Character(CharacterId::KindMita),
                ConversationSpeaker::Player,
            ]
            .into(),
            seen_by: [CharacterId::KindMita].into(),
        };
        append(&mut store, branch, message);

        let a = TimelineMessageQueries::capture(&store, branch, CharacterId::Mita, None).unwrap();
        let b =
            TimelineMessageQueries::capture(&store, branch, CharacterId::KindMita, None).unwrap();
        let (a_messages, _) = project_history(&a, CharacterId::Mita, None);
        let (b_messages, _) = project_history(&b, CharacterId::KindMita, None);
        assert_eq!(a_messages[0].role, LlmRole::Assistant);
        assert!(
            a_messages[0]
                .text
                .starts_with("[To: kind-mita; другие участники: player] ")
        );
        assert_eq!(b_messages[0].role, LlmRole::User);
        assert_eq!(
            b_messages[0].text,
            "[Собеседник: mita -> kind-mita; другие участники: player] Привет!"
        );
    }

    #[test]
    fn witnessed_retelling_never_exposes_the_original_private_message() {
        let mut store = TimelineStore::new();
        let branch = BranchId::new();
        store.create_session(SessionId::new(), branch).unwrap();

        let mut original = ConversationMessage::new(
            "private-b-to-player",
            ConversationSpeaker::Character(CharacterId::KindMita),
            json!("Только для игрока"),
            [],
        );
        original
            .audience
            .targets
            .insert(ConversationSpeaker::Player);
        let original_id = append(&mut store, branch, original);

        let mut retelling = ConversationMessage::new(
            "b-retells-private",
            ConversationSpeaker::Character(CharacterId::KindMita),
            json!("Я говорила кое-что игроку"),
            [CharacterId::Mita],
        );
        retelling
            .audience
            .targets
            .insert(ConversationSpeaker::Character(CharacterId::Mita));
        retelling.links.push(crate::timeline::domain::CausalLink {
            relation: crate::timeline::domain::CausalRelation::CausedBy,
            event_id: original_id,
        });
        let retelling_id = append(&mut store, branch, retelling);
        assert!(
            !store
                .knows(branch, CharacterId::Mita, original_id, None)
                .unwrap()
        );
        assert!(
            store
                .knows(branch, CharacterId::Mita, retelling_id, None)
                .unwrap()
        );

        let view =
            TimelineMessageQueries::capture(&store, branch, CharacterId::Mita, None).unwrap();
        let (messages, _) = project_history(&view, CharacterId::Mita, None);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, LlmRole::User);
        assert_eq!(
            messages[0].text,
            "[Собеседник: kind-mita -> mita] Я говорила кое-что игроку"
        );
    }

    #[test]
    fn prefix_preserves_multimodal_text_parts_and_legacy_player_input() {
        let mut message = Map::new();
        message.insert(
            "content".into(),
            json!([
                {"type": "text", "text": "Первый"},
                {"type": "image", "data": "hidden bytes"},
                {"type": "text", "text": "Второй"}
            ]),
        );
        assert_eq!(message_text(&message).unwrap(), "Первый\nВторой");
        assert_eq!(
            speaker_prefix(
                CharacterId::Mita,
                MessageRole::User,
                Some("player"),
                &message
            ),
            ""
        );
    }
}
