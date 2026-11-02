use serde::{Deserialize, Serialize};

use super::GeminiTemplate;
use crate::{
    character::CharacterContext,
    generation::{
        GenerationError,
        conversation::{LlmConversation, LlmRole},
        memory_context::MemoryHandoff,
        structured::{StructuredResponse, gemini_message_schema},
    },
};

#[cfg(test)]
use crate::{
    generation::conversation::{message_text, project_history},
    timeline::view::MitaView,
};

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
    response_mime_type: String,
    response_schema: serde_json::Value,
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

#[cfg(test)]
fn history_contents(view: &MitaView) -> Vec<Content> {
    project_history(view, crate::identity::CharacterId::Mita, None)
        .0
        .into_iter()
        .map(|message| {
            let role = match message.role {
                LlmRole::User => "user",
                LlmRole::Assistant => "model",
            };
            content(Some(role), &message.text)
        })
        .collect()
}

#[cfg(test)]
pub(super) fn encode(context: CharacterContext, template: &GeminiTemplate) -> GenerateContent {
    encode_with_memory(context, template, &MemoryHandoff::Lexical)
}

pub(super) fn encode_with_memory(
    context: CharacterContext,
    template: &GeminiTemplate,
    memory: &MemoryHandoff,
) -> GenerateContent {
    let conversation = LlmConversation::from_context_with_memory(&context, memory);
    GenerateContent {
        system_instruction: content(None, &conversation.system_instruction),
        contents: conversation
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    LlmRole::User => "user",
                    LlmRole::Assistant => "model",
                };
                content(Some(role), &message.text)
            })
            .collect(),
        generation_config: Config {
            temperature: template.settings().temperature(),
            max_output_tokens: template.settings().max_output_tokens(),
            candidate_count: 1,
            response_mime_type: "application/json".into(),
            response_schema: gemini_message_schema(),
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

pub(super) fn decode(bytes: &[u8]) -> Result<StructuredResponse, GenerationError> {
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
    StructuredResponse::parse(&text)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::interaction::TurnIntent;
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
                attitude: 60.0,
                boredom: 10.0,
                stress: 5.0,
            },
            working_state: Default::default(),
            memories: Vec::new(),
            history: HistoryView {
                at: ContextAnchor {
                    branch_id: branch.0,
                    through: 2,
                },
                events: vec![],
            },
            view,
            current_input: Arc::from("old question"),
            focus: GenerationFocus {
                intent: TurnIntent::Reply(ids[0]),
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
                attitude: 60.0,
                boredom: 10.0,
                stress: 5.0,
            },
            working_state: Default::default(),
            memories: Vec::new(),
            history: HistoryView {
                at: ContextAnchor {
                    branch_id: branch.0,
                    through: 3,
                },
                events: vec![],
            },
            view,
            current_input: Arc::from("Comment on the event"),
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
        let body = json!({"candidates":[{"content":{"parts":[{"text":"secret thought","thought":true},{"text":"{\"segments\":[{\"text\":\"answer\"}]}"}]},"finishReason":"STOP"}]});
        assert_eq!(
            decode(&serde_json::to_vec(&body).unwrap())
                .unwrap()
                .full_text()
                .as_ref(),
            "answer"
        );
    }

    #[test]
    fn generated_actions_are_parsed_but_not_accepted_without_executors() {
        let body = json!({"candidates":[{"content":{"parts":[{"text":
            r#"{"segments":[{"text":"hello","commands":["move"]}]}"#
        }]},"finishReason":"STOP"}]});
        let response = decode(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(response.segments[0].commands, ["move"]);
        assert!(response.ensure_supported_effects().is_err());
        let malformed = json!({"candidates":[{"content":{"parts":[{"text":
            r#"{"segments":[{"text":"hello","unexpected_effect":true}]}"#
        }]},"finishReason":"STOP"}]});
        assert!(decode(&serde_json::to_vec(&malformed).unwrap()).is_err());
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
