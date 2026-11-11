use serde::{Deserialize, Serialize};

use super::{PromptMessage, PromptRole};
use crate::identity::EventId;

/// Ordered, provider-owned transformations of the text prompt. Timeline facts
/// and character history are never modified by this pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "id",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MessageTransform {
    MergeSystemMessages,
    EnsureAlternatingRoles,
    EnsureLastMessageUser { fallback_user_text: String },
    SystemToUserPrefix { tag: String },
    SystemMessagesToUser { tag: String },
    NormalizeSystemMessages { tag: String },
    TrailingSystemToUserPrefix { tag: String },
}

/// The lineage of every text fragment survives message joins and role conversions.
/// A fragment with a source is a complete projected Timeline message, not an
/// arbitrary text span guessed after a provider transform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptFragment {
    pub(crate) text: String,
    pub(crate) source: Option<EventId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrackedPrompt {
    pub(crate) role: PromptRole,
    pub(crate) fragments: Vec<PromptFragment>,
}

impl TrackedPrompt {
    pub(crate) fn new(message: PromptMessage, source: Option<EventId>) -> Self {
        Self {
            role: message.role,
            fragments: vec![PromptFragment {
                text: message.text.to_string(),
                source,
            }],
        }
    }

    pub(crate) fn text(&self) -> String {
        self.fragments
            .iter()
            .map(|part| part.text.as_str())
            .collect()
    }

    pub(crate) fn message(&self) -> PromptMessage {
        PromptMessage {
            role: self.role,
            text: self.text().into(),
        }
    }

    pub(crate) fn prepend(&mut self, text: String) {
        self.fragments
            .insert(0, PromptFragment { text, source: None });
    }

    fn append(&mut self, text: String) {
        self.fragments.push(PromptFragment { text, source: None });
    }

    fn join(&mut self, next: Self) {
        if !self.text().is_empty() && !next.text().is_empty() {
            self.append("\n\n".into());
        }
        self.fragments.extend(next.fragments);
    }
}

fn untracked(role: PromptRole, text: impl Into<String>) -> TrackedPrompt {
    TrackedPrompt {
        role,
        fragments: vec![PromptFragment {
            text: text.into(),
            source: None,
        }],
    }
}

fn joined(messages: &[TrackedPrompt], trim: bool) -> String {
    messages
        .iter()
        .map(TrackedPrompt::text)
        .map(|text| if trim { text.trim().to_owned() } else { text })
        .filter(|s| !trim || !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn convert_system(messages: Vec<TrackedPrompt>, tag: &str) -> Vec<TrackedPrompt> {
    let leading = messages
        .iter()
        .take_while(|m| m.role == PromptRole::System)
        .count();
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(index, mut message)| {
            if message.role == PromptRole::System && message.text().trim().is_empty() {
                return None;
            }
            if index >= leading && message.role == PromptRole::System {
                message.role = PromptRole::User;
                message.prepend(format!("{tag}\n"));
            }
            Some(message)
        })
        .collect()
}

pub(crate) fn apply_tracked_transforms(
    mut messages: Vec<TrackedPrompt>,
    transforms: &[MessageTransform],
) -> Vec<TrackedPrompt> {
    for transform in transforms {
        match transform {
            MessageTransform::MergeSystemMessages => {
                let leading = messages
                    .iter()
                    .take_while(|m| m.role == PromptRole::System)
                    .count();
                if leading > 0 {
                    let text = joined(&messages[..leading], true);
                    if !text.is_empty() {
                        messages.drain(..leading);
                        messages.insert(0, untracked(PromptRole::System, text));
                    }
                }
            }
            MessageTransform::EnsureAlternatingRoles => {
                let mut merged: Vec<TrackedPrompt> = Vec::with_capacity(messages.len());
                for message in messages {
                    if let Some(last) = merged.last_mut().filter(|m| m.role == message.role) {
                        last.join(message);
                    } else {
                        merged.push(message);
                    }
                }
                messages = merged;
            }
            MessageTransform::EnsureLastMessageUser { fallback_user_text } => {
                if messages
                    .last()
                    .is_none_or(|m| m.role == PromptRole::Assistant)
                {
                    messages.push(untracked(PromptRole::User, fallback_user_text.clone()));
                }
            }
            MessageTransform::SystemToUserPrefix { tag } => {
                let context = messages
                    .iter()
                    .filter(|m| m.role == PromptRole::System)
                    .map(TrackedPrompt::text)
                    .map(|text| text.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .map(|text| format!("{tag} {text}"))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.retain(|m| m.role != PromptRole::System);
                if !context.is_empty() {
                    let context = context.trim().to_owned();
                    if let Some(message) = messages.iter_mut().find(|m| m.role == PromptRole::User)
                    {
                        message.prepend(format!("{context}\n\n"));
                    } else {
                        messages.push(untracked(PromptRole::User, context));
                    }
                }
            }
            MessageTransform::SystemMessagesToUser { tag } => {
                messages = convert_system(messages, tag);
            }
            MessageTransform::NormalizeSystemMessages { tag } => {
                messages = convert_system(messages, tag);
                let leading = messages
                    .iter()
                    .take_while(|m| m.role == PromptRole::System)
                    .count();
                if leading >= 2 {
                    let text = joined(&messages[..leading], false);
                    messages.drain(..leading);
                    messages.insert(0, untracked(PromptRole::System, text));
                }
            }
            MessageTransform::TrailingSystemToUserPrefix { tag } => {
                if let Some(message) = messages.pop_if(|m| m.role == PromptRole::System)
                    && !message.text().trim().is_empty()
                {
                    messages.push(untracked(
                        PromptRole::User,
                        format!("{tag} {}", message.text().trim()),
                    ));
                }
            }
        }
    }
    messages
}

pub fn apply_message_transforms(
    messages: Vec<PromptMessage>,
    transforms: &[MessageTransform],
) -> Vec<PromptMessage> {
    apply_tracked_transforms(
        messages
            .into_iter()
            .map(|message| TrackedPrompt::new(message, None))
            .collect(),
        transforms,
    )
    .into_iter()
    .map(|message| message.message())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agrees_with_python_reference_on_text_edge_cases() {
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/python-message-transforms.json"
        ))
        .unwrap();
        let parse_messages = |value: &serde_json::Value| {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|message| {
                    let role = match message["role"].as_str().unwrap() {
                        "system" => PromptRole::System,
                        "user" => PromptRole::User,
                        "assistant" => PromptRole::Assistant,
                        _ => panic!(),
                    };
                    PromptMessage {
                        role,
                        text: message["content"].as_str().unwrap().into(),
                    }
                })
                .collect::<Vec<_>>()
        };
        for case in cases.as_array().unwrap() {
            let transform: MessageTransform =
                serde_json::from_value(case["transform"].clone()).unwrap();
            assert_eq!(
                apply_message_transforms(parse_messages(&case["input"]), &[transform]),
                parse_messages(&case["expected"]),
                "{}",
                case["name"]
            );
        }
    }

    fn message(role: PromptRole, text: &str) -> PromptMessage {
        PromptMessage {
            role,
            text: text.into(),
        }
    }

    #[test]
    fn normalization_preserves_position_and_merges_only_leading_instructions() {
        use PromptRole::*;
        let input = vec![
            message(System, " first "),
            message(System, "second"),
            message(User, "hello"),
            message(Assistant, "answer"),
            message(System, "now"),
            message(User, "again"),
        ];
        let output = apply_message_transforms(
            input.clone(),
            &[MessageTransform::NormalizeSystemMessages {
                tag: "[SYSTEM INFO]".into(),
            }],
        );
        assert_eq!(
            output,
            vec![
                message(System, " first \n\nsecond"),
                message(User, "hello"),
                message(Assistant, "answer"),
                message(User, "[SYSTEM INFO]\nnow"),
                message(User, "again")
            ]
        );
        assert_eq!(input.len(), 6);
    }

    #[test]
    fn ordered_pipeline_merges_roles_then_appends_explicit_user_prompt() {
        use PromptRole::*;
        let output = apply_message_transforms(
            vec![
                message(System, "instructions"),
                message(User, "one"),
                message(User, "two"),
                message(Assistant, "reply"),
            ],
            &[
                MessageTransform::SystemToUserPrefix {
                    tag: "[SYSTEM CONTEXT]".into(),
                },
                MessageTransform::EnsureAlternatingRoles,
                MessageTransform::EnsureLastMessageUser {
                    fallback_user_text: ".".into(),
                },
            ],
        );
        assert_eq!(
            output,
            vec![
                message(User, "[SYSTEM CONTEXT] instructions\n\none\n\ntwo"),
                message(Assistant, "reply"),
                message(User, ".")
            ]
        );
    }

    #[test]
    fn empty_and_system_ending_prompts_follow_python_semantics() {
        use PromptRole::*;
        let ensure = MessageTransform::EnsureLastMessageUser {
            fallback_user_text: ".".into(),
        };
        assert_eq!(
            apply_message_transforms(vec![], std::slice::from_ref(&ensure)),
            vec![message(User, ".")]
        );
        assert_eq!(
            apply_message_transforms(vec![message(System, "keep")], &[ensure]),
            vec![message(System, "keep")]
        );
        assert!(
            apply_message_transforms(
                vec![message(System, " ")],
                &[MessageTransform::TrailingSystemToUserPrefix { tag: "tag".into() }]
            )
            .is_empty()
        );
    }

    #[test]
    fn alternating_roles_preserves_distinct_event_ids_for_identical_text() {
        let first = EventId::new();
        let second = EventId::new();
        let messages = vec![
            TrackedPrompt::new(
                super::super::PromptMessage {
                    role: PromptRole::System,
                    text: "trusted instruction".into(),
                },
                None,
            ),
            TrackedPrompt::new(
                super::super::PromptMessage {
                    role: PromptRole::User,
                    text: "same".into(),
                },
                Some(first),
            ),
            TrackedPrompt::new(
                super::super::PromptMessage {
                    role: PromptRole::User,
                    text: "same".into(),
                },
                Some(second),
            ),
        ];
        let transformed =
            apply_tracked_transforms(messages, &[MessageTransform::EnsureAlternatingRoles]);
        assert_eq!(transformed.len(), 2);
        assert_eq!(transformed[0].role, PromptRole::System);
        assert!(
            transformed[0]
                .fragments
                .iter()
                .all(|part| part.source.is_none())
        );
        assert_eq!(transformed[1].text(), "same\n\nsame");
        assert_eq!(
            transformed[1]
                .fragments
                .iter()
                .filter_map(|part| part.source)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
    }

    #[test]
    fn moved_system_prefix_never_inherits_user_event_provenance() {
        let origin = EventId::new();
        let transformed = apply_tracked_transforms(
            vec![
                TrackedPrompt::new(
                    super::super::PromptMessage {
                        role: PromptRole::System,
                        text: "policy".into(),
                    },
                    None,
                ),
                TrackedPrompt::new(
                    super::super::PromptMessage {
                        role: PromptRole::User,
                        text: "media".into(),
                    },
                    Some(origin),
                ),
            ],
            &[MessageTransform::SystemToUserPrefix {
                tag: "[INSTRUCTION]".into(),
            }],
        );
        assert_eq!(transformed.len(), 1);
        assert_eq!(transformed[0].role, PromptRole::User);
        assert_eq!(transformed[0].text(), "[INSTRUCTION] policy\n\nmedia");
        assert_eq!(transformed[0].fragments[0].source, None);
        assert_eq!(transformed[0].fragments[1].source, Some(origin));
    }

    #[test]
    fn unknown_transform_or_parameter_is_rejected() {
        assert!(serde_json::from_str::<MessageTransform>(r#"{"id":"unknown"}"#).is_err());
        assert!(serde_json::from_str::<MessageTransform>(r#"{"id":"ensure_last_message_user","params":{"fallback_user_text":".","typo":true}}"#).is_err());
    }
}
