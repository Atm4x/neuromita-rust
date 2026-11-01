use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::GeminiTemplate;
use crate::{
    character::CharacterContext,
    generation::GenerationError,
    interaction::TurnIntent,
    timeline::{
        domain::MessageRole,
        view::{ContextEntry, MitaView},
    },
};
use serde_json::Value;

use crate::identity::EventId;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GenerateContent {
    system_instruction: Content,
    contents: Vec<Content>,
    generation_config: Config,
}

#[derive(Serialize, Deserialize)]
struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<Part>,
}

#[derive(Serialize, Deserialize)]
struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    temperature: f32,
    max_output_tokens: u32,
    candidate_count: u32,
}

fn content(role: Option<&str>, text: &str) -> Content {
    Content {
        role: role.map(str::to_owned),
        parts: vec![Part {
            text: Some(text.to_owned()),
            thought: None,
        }],
    }
}

fn message_text(fields: &serde_json::Map<String, Value>) -> Option<String> {
    match fields.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| part.as_object())
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            Some(texts.join("\n"))
        }
        _ => None,
    }
}

#[cfg(test)]
fn history_contents(view: &MitaView) -> Vec<Content> {
    history_contents_with_focus(view, None).0
}

fn history_contents_with_focus(
    view: &MitaView,
    target: Option<(EventId, TurnIntent)>,
) -> (Vec<Content>, bool) {
    let entries = view.context_entries();
    let target_id = target.map(|(id, _)| id);
    let position = entries.iter().position(|entry| match entry {
        ContextEntry::Message(message) => Some(message.event_id) == target_id,
        ContextEntry::World(event) => Some(event.event_id) == target_id,
    });
    let focus_needs_marker = position.is_some_and(|index| {
        target.is_some_and(|(_, intent)| matches!(intent, TurnIntent::React(_)))
            || index + 1 < entries.len()
    });
    let mut contents = Vec::with_capacity(entries.len());
    let mut target_present = false;
    for entry in entries {
        let (id, role, mut text) = match entry {
            ContextEntry::Message(message) => {
                let Some(text) = message_text(&message.fields) else {
                    continue;
                };
                let role = match message.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "model",
                    MessageRole::System => "user",
                };
                let text = if message.role == MessageRole::System {
                    format!("Сведения сессии:\n{text}")
                } else {
                    text
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
                    "user",
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
                let label =
                    if target.is_some_and(|(_, intent)| matches!(intent, TurnIntent::React(_))) {
                        "Событие, вызвавшее текущую реакцию"
                    } else {
                        "Сообщение, на которое готовится текущий ответ"
                    };
                text = format!("{label}:\n{text}");
            }
        }
        contents.push(content(Some(role), &text));
    }
    (contents, target_present)
}

pub(super) fn encode(context: CharacterContext, template: &GeminiTemplate) -> GenerateContent {
    let focus = match context.focus.intent {
        TurnIntent::Reply => Some((context.trigger, context.focus.intent)),
        TurnIntent::React(id) => Some((id, context.focus.intent)),
        TurnIntent::Initiate => None,
    };
    let (mut contents, target_present) = history_contents_with_focus(&context.view, focus);
    match context.focus.intent {
        TurnIntent::Reply if !target_present => {
            contents.push(content(Some("user"), &context.current_input));
        }
        TurnIntent::React(_) => {
            let task = if target_present {
                format!(
                    "Текущая задача — отреагировать на отмеченное событие. Учитывай и более поздние известные сведения.\n{}",
                    context.current_input
                )
            } else if let Some(cause) = &context.focus.reaction {
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
            };
            contents.push(content(Some("user"), &task));
        }
        TurnIntent::Initiate => {
            contents.push(content(Some("user"), &context.current_input));
        }
        TurnIntent::Reply => {}
    }
    GenerateContent {
        system_instruction: content(
            None,
            &format!(
                "{}\n{}\nПринятых ответов: {}.",
                context.description, context.prompt, context.state.accepted_replies
            ),
        ),
        contents,
        generation_config: Config {
            temperature: template.settings().temperature(),
            max_output_tokens: template.settings().max_output_tokens(),
            candidate_count: 1,
        },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: Option<Content>,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    block_reason: Option<String>,
}

pub(super) fn decode(bytes: &[u8]) -> Result<Arc<str>, GenerationError> {
    let response: GenerateResponse = serde_json::from_slice(bytes)
        .map_err(|_| GenerationError::Response("invalid Gemini JSON".into()))?;
    if response
        .prompt_feedback
        .and_then(|feedback| feedback.block_reason)
        .is_some()
    {
        return Err(GenerationError::Response(
            "Gemini blocked the prompt".into(),
        ));
    }
    if response.candidates.len() != 1 {
        return Err(GenerationError::Response(
            "expected exactly one candidate".into(),
        ));
    }
    let candidate = response
        .candidates
        .into_iter()
        .next()
        .expect("one candidate checked");
    if candidate.finish_reason.as_deref() != Some("STOP") {
        return Err(GenerationError::Response(format!(
            "generation did not finish successfully: {:?}",
            candidate.finish_reason
        )));
    }
    let content = candidate
        .content
        .ok_or_else(|| GenerationError::Response("candidate has no content".into()))?;
    let text: String = content
        .parts
        .into_iter()
        .filter(|part| part.thought != Some(true))
        .filter_map(|part| part.text)
        .collect();
    if text.trim().is_empty() {
        return Err(GenerationError::Response(
            "candidate has no answer text".into(),
        ));
    }
    Ok(Arc::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_history_follows_character_visibility_and_message_semantics() {
        use crate::{
            identity::CharacterId,
            timeline::{
                domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
                view::TimelineMessageQueries,
            },
        };
        let mut repository = TimelineStore::new();
        let branch = BranchId::new();
        repository.create_session(SessionId::new(), branch).unwrap();
        fn add(repository: &mut TimelineStore, branch: BranchId, key: &str, draft: EventDraft) {
            let head = repository.branch(branch).unwrap().head_sequence;
            repository
                .append(branch, head, key, json!({}), vec![draft])
                .unwrap();
        }
        let mut private = EventDraft::new(EventKind::PlayerMessage);
        private.observers.insert(CharacterId::Mita);
        private.actor = Some("player".into());
        private.payload.insert("content".into(), json!("secret"));
        let secret_id = private.id;
        add(&mut repository, branch, "private", private);
        let mut other = EventDraft::new(EventKind::PlayerMessage);
        other.observers.insert(CharacterId::KindMita);
        other.actor = Some("player".into());
        other
            .payload
            .insert("content".into(), json!("other secret"));
        add(&mut repository, branch, "other", other);
        let mut assistant = EventDraft::new(EventKind::CharacterMessage);
        assistant.observers.insert(CharacterId::Mita);
        assistant.actor = Some(CharacterId::Mita.to_string());
        assistant.payload.insert("content".into(), json!("I know"));
        add(&mut repository, branch, "assistant", assistant);
        let mut note = EventDraft::new(EventKind::SessionNote);
        note.observers.insert(CharacterId::Mita);
        note.payload
            .insert("content".into(), json!("private world note"));
        add(&mut repository, branch, "note", note);
        let visible =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Mita, None).unwrap();
        let contents = history_contents(&visible);
        let encoded: Vec<_> = contents
            .iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect();
        assert_eq!(encoded.len(), 3);
        assert_eq!(encoded[0]["role"], "user");
        assert_eq!(encoded[0]["parts"][0]["text"], "secret");
        assert_eq!(encoded[1]["role"], "model");
        assert_eq!(encoded[1]["parts"][0]["text"], "I know");
        assert_eq!(encoded[2]["role"], "user");
        assert_eq!(
            encoded[2]["parts"][0]["text"],
            "Сведения сессии:\nprivate world note"
        );
        assert!(!format!("{encoded:?}").contains("other secret"));

        let mut hidden = EventDraft::new(EventKind::MessageHidden);
        hidden.observers.insert(CharacterId::Mita);
        hidden
            .payload
            .insert("event_ids".into(), json!([secret_id.0.to_string()]));
        add(&mut repository, branch, "hide", hidden);
        let after_hide =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Mita, None).unwrap();
        let contents = history_contents(&after_hide);
        assert_eq!(contents.len(), 2);
        assert_eq!(serde_json::to_value(&contents[0]).unwrap()["role"], "model");

        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Mita);
        add(&mut repository, branch, "reset", reset);
        let after_reset =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Mita, None).unwrap();
        let contents = history_contents(&after_reset);
        assert!(contents.is_empty());
    }

    #[test]
    fn reactive_provider_history_keeps_questions_after_the_cause() {
        use crate::{
            identity::CharacterId,
            timeline::{
                domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
                view::TimelineMessageQueries,
            },
        };

        let mut repository = TimelineStore::new();
        let branch = BranchId::new();
        repository.create_session(SessionId::new(), branch).unwrap();
        for (key, kind, content) in [
            ("door", EventKind::CharacterObservation, "Door opened"),
            ("question", EventKind::PlayerMessage, "Why did you open it?"),
            ("weather", EventKind::SessionNote, "Rain started"),
        ] {
            let mut draft = EventDraft::new(kind);
            match kind {
                EventKind::PlayerMessage => draft.actor = Some("player".into()),
                EventKind::CharacterObservation => {
                    draft.actor = Some(CharacterId::Mita.to_string())
                }
                _ => {}
            }
            draft.observers.insert(CharacterId::Mita);
            draft.payload.insert("content".into(), json!(content));
            let head = repository.branch(branch).unwrap().head_sequence;
            repository
                .append(branch, head, key, json!({}), vec![draft])
                .unwrap();
        }

        let before_question =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Mita, Some(1))
                .unwrap();
        assert_eq!(history_contents(&before_question).len(), 1);

        let when_generation_starts =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Mita, None).unwrap();
        let encoded: Vec<_> = history_contents(&when_generation_starts)
            .iter()
            .map(|message| serde_json::to_value(message).unwrap())
            .collect();
        assert_eq!(encoded.len(), 3);
        assert_eq!(encoded[0]["parts"][0]["text"], "Door opened");
        assert_eq!(encoded[1]["parts"][0]["text"], "Why did you open it?");
        assert_eq!(
            encoded[2]["parts"][0]["text"],
            "Сведения сессии:\nRain started"
        );
    }

    #[test]
    fn reply_focus_marks_old_target_without_dropping_newer_messages() {
        use crate::{
            character::{CharacterState, GenerationFocus},
            identity::CharacterId,
            timeline::{
                ContextAnchor, HistoryView,
                domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
                selection::{TimelineSelection, TimelineTurn},
                view::TimelineMessageQueries,
            },
        };
        let branch = BranchId::new();
        let mut store = TimelineStore::new();
        store.create_session(SessionId::new(), branch).unwrap();
        let mut ids = Vec::new();
        for (key, content) in [("target", "old question"), ("newer", "new question")] {
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Mita);
            draft.payload.insert("content".into(), json!(content));
            ids.push(draft.id);
            let head = store.branch(branch).unwrap().head_sequence;
            store
                .append(branch, head, key, json!({}), vec![draft])
                .unwrap();
        }
        let view =
            TimelineMessageQueries::capture(&store, branch, CharacterId::Mita, None).unwrap();
        let context = CharacterContext {
            character_id: CharacterId::Mita,
            description: Arc::from("Mita"),
            prompt: Arc::from("Speak naturally"),
            state: CharacterState {
                accepted_replies: 0,
                revision: 0,
            },
            history: HistoryView {
                at: ContextAnchor {
                    branch_id: branch.0,
                    through: 2,
                },
                events: vec![],
            },
            view,
            current_input: Arc::from("old question"),
            trigger: ids[0],
            focus: GenerationFocus {
                intent: TurnIntent::Reply,
                reaction: None,
            },
            anchor: ContextAnchor {
                branch_id: branch.0,
                through: 2,
            },
            turn: TimelineTurn {
                selection: TimelineSelection {
                    branch_id: branch,
                    generation: 1,
                },
                context_head: 2,
                character: CharacterId::Mita,
                context_epoch: 0,
                trigger: Some(ids[0]),
            },
        };
        let template = GeminiTemplate::google(
            "test-model",
            "test-key",
            super::super::GenerationSettings::new(0.5, 64, std::time::Duration::from_secs(2))
                .unwrap(),
        )
        .unwrap();
        let encoded = serde_json::to_value(encode(context, &template)).unwrap();
        let contents = encoded["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert!(
            contents[0]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("old question")
        );
        assert!(
            contents[0]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("на которое готовится текущий ответ")
        );
        assert_eq!(contents[1]["parts"][0]["text"], "new question");
        assert_eq!(encoded.to_string().matches("old question").count(), 1);
    }

    #[test]
    fn reaction_focus_includes_non_message_cause_without_leaking_other_characters() {
        use crate::{
            character::{CharacterState, GenerationFocus, ReactionCause},
            identity::CharacterId,
            timeline::{
                ContextAnchor, HistoryView,
                domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
                selection::{TimelineSelection, TimelineTurn},
                view::TimelineMessageQueries,
            },
        };
        let branch = BranchId::new();
        let mut store = TimelineStore::new();
        store.create_session(SessionId::new(), branch).unwrap();
        let mut cause = EventDraft::new(EventKind::UnityEvent);
        cause.observers.insert(CharacterId::Mita);
        cause.payload.insert("content".into(), json!("door opened"));
        let cause_id = cause.id;
        let mut question = EventDraft::new(EventKind::PlayerMessage);
        question.observers.insert(CharacterId::Mita);
        question.actor = Some("player".into());
        question
            .payload
            .insert("content".into(), json!("Why did the door open?"));
        let mut secret = EventDraft::new(EventKind::PlayerMessage);
        secret.observers.insert(CharacterId::KindMita);
        secret.actor = Some("player".into());
        secret
            .payload
            .insert("content".into(), json!("only KindMita knows"));
        for (key, draft) in [
            ("cause", cause),
            ("question", question),
            ("private", secret),
        ] {
            let head = store.branch(branch).unwrap().head_sequence;
            store
                .append(branch, head, key, json!({}), vec![draft])
                .unwrap();
        }
        let view =
            TimelineMessageQueries::capture(&store, branch, CharacterId::Mita, None).unwrap();
        let context = CharacterContext {
            character_id: CharacterId::Mita,
            description: Arc::from("Mita"),
            prompt: Arc::from("Speak naturally"),
            state: CharacterState {
                accepted_replies: 0,
                revision: 0,
            },
            history: HistoryView {
                at: ContextAnchor {
                    branch_id: branch.0,
                    through: 3,
                },
                events: vec![],
            },
            view,
            current_input: Arc::from("Comment on the event"),
            trigger: EventId::new(),
            focus: GenerationFocus {
                intent: TurnIntent::React(cause_id),
                reaction: Some(ReactionCause {
                    event_id: cause_id,
                    kind: EventKind::UnityEvent,
                    description: Some(Arc::from("door opened")),
                }),
            },
            anchor: ContextAnchor {
                branch_id: branch.0,
                through: 3,
            },
            turn: TimelineTurn {
                selection: TimelineSelection {
                    branch_id: branch,
                    generation: 1,
                },
                context_head: 3,
                character: CharacterId::Mita,
                context_epoch: 0,
                trigger: None,
            },
        };
        let template = GeminiTemplate::google(
            "test-model",
            "test-key",
            super::super::GenerationSettings::new(0.5, 64, std::time::Duration::from_secs(2))
                .unwrap(),
        )
        .unwrap();
        let encoded = serde_json::to_value(encode(context, &template)).unwrap();
        let user = encoded["contents"].as_array().unwrap();
        assert_eq!(user.len(), 3);
        let world = user[0]["parts"][0]["text"].as_str().unwrap();
        assert!(world.contains("Событие, вызвавшее текущую реакцию"));
        assert!(world.contains("UnityEvent"));
        assert!(world.contains("door opened"));
        assert_eq!(user[0]["role"], "user");
        assert_eq!(user[1]["parts"][0]["text"], "Why did the door open?");
        let last = user[2]["parts"][0]["text"].as_str().unwrap();
        assert!(last.contains("отмеченное событие"));
        assert!(last.contains("Comment on the event"));
        assert_eq!(encoded.to_string().matches("door opened").count(), 1);
        assert!(!encoded.to_string().contains("only KindMita knows"));
    }

    #[test]
    fn array_content_is_reduced_to_text_without_serializing_unsupported_media() {
        let fields = json!({"content": [
            {"type":"text", "text":"hello"},
            {"type":"image", "data":"private-image-bytes"},
            {"type":"text", "text":"world"}
        ]});
        assert_eq!(
            message_text(fields.as_object().unwrap()),
            Some("hello\nworld".into())
        );
    }

    #[test]
    fn reasoning_is_not_returned_as_answer() {
        let body = json!({"candidates":[{"content":{"parts":[{"text":"secret thought","thought":true},{"text":"answer"}]},"finishReason":"STOP"}]});
        assert_eq!(
            &*decode(&serde_json::to_vec(&body).unwrap()).unwrap(),
            "answer"
        );
    }

    #[test]
    fn blocked_truncated_and_empty_answers_are_errors() {
        for body in [
            json!({"promptFeedback":{"blockReason":"SAFETY"}}),
            json!({"candidates":[{"content":{"parts":[{"text":"partial"}]},"finishReason":"MAX_TOKENS"}]}),
            json!({"candidates":[{"content":{"parts":[{"text":"only thought","thought":true}]},"finishReason":"STOP"}]}),
            json!({"candidates":[]}),
            json!({"candidates":[{"finishReason":"STOP"}]}),
        ] {
            assert!(decode(&serde_json::to_vec(&body).unwrap()).is_err());
        }
        assert!(decode(b"not json").is_err());
    }
}
