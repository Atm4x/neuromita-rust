use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::GeminiTemplate;
use crate::{
    character::CharacterContext,
    generation::GenerationError,
    timeline::{domain::MessageRole, view::MitaView},
};
use serde_json::Value;

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

fn history_contents(view: &MitaView) -> Vec<Content> {
    let mut contents = Vec::new();
    for message in view.messages() {
        let Some(text) = message_text(&message.fields) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        match message.role {
            MessageRole::User => contents.push(content(Some("user"), &text)),
            MessageRole::Assistant => contents.push(content(Some("model"), &text)),
            MessageRole::System => {
                contents.push(content(Some("user"), &format!("Сведения сессии:\n{text}")));
            }
        }
    }
    contents
}

pub(super) fn encode(context: CharacterContext, template: &GeminiTemplate) -> GenerateContent {
    let mut contents = history_contents(&context.view);
    contents.push(content(Some("user"), &context.current_input));
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
