//! Event-anchored media routing through the existing text prompt pipeline.
//! A transform may alter text-only context, but it must never silently rebind
//! media bytes to another message or role.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::{
    GenerationError, PromptRole,
    conversation::{LlmConversation, LlmRole},
};
use crate::{
    identity::EventId,
    timeline::{domain::TimelineMessagePart, storage::assets::TimelineAssetStore, view::MitaView},
};

#[derive(Clone, Copy)]
pub(crate) enum LiveDialect {
    Gemini,
    OpenAiChat,
    OpenAiResponses,
}

pub(crate) struct LiveMedia {
    entries: HashMap<EventId, MediaEntry>,
}

struct MediaEntry {
    role: PromptRole,
    projected_text: String,
    parts: Vec<Value>,
}

impl LiveMedia {
    pub(crate) fn prepare(
        conversation: &LlmConversation,
        view: &MitaView,
        assets: Option<&TimelineAssetStore>,
        dialect: LiveDialect,
    ) -> Result<Option<Self>, GenerationError> {
        if conversation.source_anchor.branch_id != view.branch.0
            || conversation.source_anchor.through != view.through
        {
            return Err(GenerationError::Response(
                "generation history and media view have different Timeline anchors".into(),
            ));
        }
        let known = view
            .messages()
            .into_iter()
            .map(|message| (message.event_id, message))
            .collect::<HashMap<_, _>>();
        let mut affected = Vec::new();
        for projected in &conversation.messages {
            let Some(id) = projected.source else { continue };
            let original = known.get(&id).ok_or_else(|| {
                GenerationError::Response(
                    "Timeline event is not visible in the generation snapshot".into(),
                )
            })?;
            let parts = original
                .fields
                .get("content")
                .map(crate::timeline::domain::parse_message_content)
                .transpose()
                .map_err(|error| {
                    GenerationError::Response(format!("invalid visible message: {error}"))
                })?
                .unwrap_or_default();
            if parts.iter().any(|part| part.asset().is_some()) {
                let raw = parts
                    .iter()
                    .filter_map(|part| match part {
                        TimelineMessagePart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let prefix = projected.text.strip_suffix(&raw).ok_or_else(|| {
                    GenerationError::Response(
                        "multimodal history prefix does not match canonical content".into(),
                    )
                })?;
                affected.push((id, prefix.to_owned(), projected));
            }
        }
        if affected.is_empty() {
            return Ok(None);
        }
        let assets = assets.ok_or_else(|| {
            GenerationError::Response(
                "Timeline assets are not configured for multimodal generation".into(),
            )
        })?;
        let selected = affected
            .iter()
            .map(|(id, _, _)| *id)
            .collect::<HashSet<_>>();
        if selected.len() != affected.len() {
            return Err(GenerationError::Response(
                "duplicate Timeline media event in prompt".into(),
            ));
        }
        let encoded = match dialect {
            LiveDialect::Gemini => {
                super::multimodal::gemini_selected_contents(view, assets, &selected)
            }
            LiveDialect::OpenAiChat => super::multimodal::openai_selected_contents(
                view,
                assets,
                super::multimodal::OpenAiImageDialect::ChatCompletions,
                &selected,
            ),
            LiveDialect::OpenAiResponses => super::multimodal::openai_selected_contents(
                view,
                assets,
                super::multimodal::OpenAiImageDialect::Responses,
                &selected,
            ),
        }
        .map_err(|error| {
            GenerationError::Response(format!("multimodal provider input rejected: {error}"))
        })?;
        let mut entries = HashMap::new();
        for (id, prefix, projected) in affected {
            let mut parts = encoded[&id][if matches!(dialect, LiveDialect::Gemini) {
                "parts"
            } else {
                "content"
            }]
            .as_array()
            .cloned()
            .ok_or_else(|| GenerationError::Response("invalid multimodal codec output".into()))?;
            if !prefix.is_empty() {
                let initial = match dialect {
                    LiveDialect::Gemini => json!({"text":prefix}),
                    LiveDialect::OpenAiChat => json!({"type":"text","text":prefix}),
                    LiveDialect::OpenAiResponses => json!({"type":"input_text","text":prefix}),
                };
                parts.insert(0, initial);
            }
            let role = match projected.role {
                LlmRole::User => PromptRole::User,
                LlmRole::Assistant => PromptRole::Assistant,
            };
            if entries
                .insert(
                    id,
                    MediaEntry {
                        role,
                        projected_text: projected.text.clone(),
                        parts,
                    },
                )
                .is_some()
            {
                return Err(GenerationError::Response(
                    "duplicate Timeline media event in prompt".into(),
                ));
            }
        }
        Ok(Some(Self { entries }))
    }

    /// Bind media by source EventId, never by a coincidentally equal text window.
    /// All targets are validated before a single wire message is mutated.
    pub(crate) fn attach_tracked(
        &self,
        prompt: &[super::transforms::TrackedPrompt],
        contents: &mut [Value],
        dialect: LiveDialect,
    ) -> Result<(), GenerationError> {
        let invalid = || {
            GenerationError::Response(
                "multimodal event provenance no longer matches the provider prompt".into(),
            )
        };
        let expected_len = match dialect {
            LiveDialect::Gemini => prompt
                .iter()
                .filter(|m| m.role != PromptRole::System)
                .count(),
            LiveDialect::OpenAiChat | LiveDialect::OpenAiResponses => prompt.len(),
        };
        if contents.len() < expected_len {
            return Err(invalid());
        }
        let mut replacements = Vec::new();
        let mut seen = HashSet::new();
        let mut wire_index = 0;
        for message in prompt {
            if matches!(dialect, LiveDialect::Gemini) && message.role == PromptRole::System {
                continue;
            }
            let wire = &contents[wire_index];
            let expected_role = match (dialect, message.role) {
                (LiveDialect::Gemini, PromptRole::Assistant) => "model",
                (LiveDialect::Gemini, _) => "user",
                (_, PromptRole::Assistant) => "assistant",
                (_, PromptRole::User) => "user",
                (_, PromptRole::System) => "system",
            };
            let wire_text = match dialect {
                LiveDialect::Gemini | LiveDialect::OpenAiResponses => {
                    wire[if matches!(dialect, LiveDialect::Gemini) {
                        "parts"
                    } else {
                        "content"
                    }]
                    .as_array()
                    .and_then(|parts| parts.first())
                    .and_then(|part| part.get("text"))
                    .and_then(Value::as_str)
                }
                LiveDialect::OpenAiChat => wire["content"].as_str(),
            };
            if wire["role"].as_str() != Some(expected_role)
                || wire_text != Some(message.text().as_str())
            {
                return Err(invalid());
            }
            let mut parts = Vec::new();
            let mut has_media = false;
            for fragment in &message.fragments {
                if let Some((id, entry)) = fragment
                    .source
                    .and_then(|id| self.entries.get(&id).map(|entry| (id, entry)))
                {
                    if !seen.insert(id)
                        || message.role != entry.role
                        || fragment.text != entry.projected_text
                        || message.role == PromptRole::System
                    {
                        return Err(invalid());
                    }
                    has_media = true;
                    parts.extend_from_slice(&entry.parts);
                } else if !fragment.text.is_empty() {
                    parts.push(match dialect {
                        LiveDialect::Gemini => json!({"text":fragment.text}),
                        LiveDialect::OpenAiChat => json!({"type":"text","text":fragment.text}),
                        LiveDialect::OpenAiResponses => {
                            json!({"type":"input_text","text":fragment.text})
                        }
                    });
                }
            }
            if has_media {
                replacements.push((wire_index, parts));
            }
            wire_index += 1;
        }
        if seen.len() != self.entries.len() {
            return Err(invalid());
        }
        for (index, parts) in replacements {
            let key = if matches!(dialect, LiveDialect::Gemini) {
                "parts"
            } else {
                "content"
            };
            contents[index][key] = Value::Array(parts);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::PromptMessage;
    use super::*;
    use crate::{
        identity::CharacterId,
        timeline::{
            domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
            storage::assets::TimelineAssetStore,
            view::TimelineMessageQueries,
        },
    };
    use std::{fs, io::Cursor};
    use uuid::Uuid;

    fn environment(content: Value) -> (std::path::PathBuf, TimelineAssetStore, MitaView, EventId) {
        let path = std::env::temp_dir().join(format!("nm-live-media-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(path.join("assets"), 8 * 1024 * 1024).unwrap();
        let mut repo = TimelineStore::new();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut draft = EventDraft::new(EventKind::PlayerMessage);
        draft.observers.insert(CharacterId::Crazy);
        draft.actor = Some("player".into());
        draft.payload.insert("content".into(), content);
        let id = draft.id;
        repo.append(branch, 0, "fixture", json!({}), vec![draft])
            .unwrap();
        let view =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
        (path, assets, view, id)
    }

    fn conversation(view: &MitaView) -> LlmConversation {
        let (messages, _) =
            super::super::conversation::project_history(view, CharacterId::Crazy, None);
        LlmConversation {
            source_anchor: crate::timeline::ContextAnchor {
                branch_id: view.branch.0,
                through: view.through,
            },
            system_instruction: "instruction".into(),
            messages,
        }
    }

    fn prompt(
        conversation: &LlmConversation,
        leading: &str,
    ) -> Vec<super::super::transforms::TrackedPrompt> {
        use super::super::transforms::TrackedPrompt;
        std::iter::once(TrackedPrompt::new(
            PromptMessage {
                role: PromptRole::System,
                text: leading.into(),
            },
            None,
        ))
        .chain(conversation.messages.iter().map(|message| {
            TrackedPrompt::new(
                PromptMessage {
                    role: match message.role {
                        LlmRole::User => PromptRole::User,
                        LlmRole::Assistant => PromptRole::Assistant,
                    },
                    text: message.text.as_str().into(),
                },
                message.source,
            )
        }))
        .collect()
    }

    #[test]
    fn mixed_parts_keep_event_identity_order_and_prefix() {
        let root = std::env::temp_dir().join(format!("nm-media-staging-{}", Uuid::new_v4()));
        let store = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let asset = store
            .import("image/png", Cursor::new(b"image-real"))
            .unwrap();
        let (path, _other, view, event) = environment(json!([
            {"type":"text", "text":"alpha"}, {"type":"image","asset":asset},
            {"type":"text", "text":"omega"}
        ]));
        let mut c = conversation(&view);
        assert_eq!(c.messages[0].source, Some(event));
        c.messages[0].text = format!("Event focus: {}", c.messages[0].text);
        let media = LiveMedia::prepare(&c, &view, Some(&store), LiveDialect::Gemini)
            .unwrap()
            .unwrap();
        let p = prompt(&c, "system");
        let mut contents = vec![json!({"role":"user","parts":[{"text":c.messages[0].text}]})];
        media
            .attach_tracked(&p, &mut contents, LiveDialect::Gemini)
            .unwrap();
        assert_eq!(contents[0]["parts"][0]["text"], "Event focus: ");
        assert_eq!(contents[0]["parts"][1]["text"], "alpha");
        assert_eq!(
            contents[0]["parts"][2]["inlineData"]["data"],
            "aW1hZ2UtcmVhbA=="
        );
        assert_eq!(contents[0]["parts"][3]["text"], "omega");
        fs::remove_dir_all(path).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn attachment_only_is_not_dropped_and_missing_assets_fail_closed() {
        let root = std::env::temp_dir().join(format!("nm-media-only-{}", Uuid::new_v4()));
        let store = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let asset = store.import("image/png", Cursor::new(b"img")).unwrap();
        let (path, _unused, view, _) = environment(json!([{"type":"image","asset":asset}]));
        let c = conversation(&view);
        assert_eq!(c.messages.len(), 1);
        assert_eq!(c.messages[0].text, "");
        assert!(LiveMedia::prepare(&c, &view, None, LiveDialect::Gemini).is_err());
        let media = LiveMedia::prepare(&c, &view, Some(&store), LiveDialect::OpenAiResponses)
            .unwrap()
            .unwrap();
        let p = prompt(&c, "system");
        let mut contents = vec![
            json!({"role":"system","content":[{"type":"input_text","text":"system"}]}),
            json!({"role":"user","content":[{"type":"input_text","text":""}]}),
        ];
        media
            .attach_tracked(&p, &mut contents, LiveDialect::OpenAiResponses)
            .unwrap();
        assert_eq!(contents[0]["role"], "system");
        assert_eq!(contents[0]["content"][0]["text"], "system");
        assert_eq!(contents[1]["content"][0]["type"], "input_image");
        assert!(
            contents[1]["content"][0]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        fs::remove_dir_all(path).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn transform_merges_or_ambiguous_matching_never_rebinds_media() {
        let root = std::env::temp_dir().join(format!("nm-media-transform-{}", Uuid::new_v4()));
        let store = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let asset = store.import("image/png", Cursor::new(b"img")).unwrap();
        let (path, _unused, view, _) =
            environment(json!([{"type":"text","text":"repeat"}, {"type":"image","asset":asset}]));
        let c = conversation(&view);
        let media = LiveMedia::prepare(&c, &view, Some(&store), LiveDialect::OpenAiChat)
            .unwrap()
            .unwrap();
        let mut contents = vec![json!({"role":"user","content":"changed"})];
        let changed = vec![super::super::transforms::TrackedPrompt::new(
            PromptMessage {
                role: PromptRole::User,
                text: "changed".into(),
            },
            None,
        )];
        assert!(
            media
                .attach_tracked(&changed, &mut contents, LiveDialect::OpenAiChat)
                .is_err()
        );
        let event_id = c.messages[0].source.unwrap();
        let matching = (0..2)
            .map(|_| {
                super::super::transforms::TrackedPrompt::new(
                    PromptMessage {
                        role: PromptRole::User,
                        text: c.messages[0].text.clone().into(),
                    },
                    Some(event_id),
                )
            })
            .collect::<Vec<_>>();
        let mut contents = vec![json!({"content":"repeat"}), json!({"content":"repeat"})];
        assert!(
            media
                .attach_tracked(&matching, &mut contents, LiveDialect::OpenAiChat)
                .is_err()
        );
        fs::remove_dir_all(path).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn media_wire_indices_preserve_system_context_and_validate_all_targets_before_writing() {
        let root = std::env::temp_dir().join(format!("nm-media-role-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let image = assets.import("image/png", Cursor::new(b"image")).unwrap();
        let (path, _unused, view, _) = environment(json!([
            {"type":"text","text":"question"}, {"type":"image","asset":image}
        ]));
        let mut conversation = conversation(&view);
        conversation
            .messages
            .push(super::super::conversation::LlmMessage {
                role: LlmRole::User,
                text: "another".into(),
                source: None,
            });
        let media =
            LiveMedia::prepare(&conversation, &view, Some(&assets), LiveDialect::OpenAiChat)
                .unwrap()
                .unwrap();
        let prompt = prompt(&conversation, "protected system instruction");
        let mut wire = vec![
            json!({"role":"system","content":"protected system instruction"}),
            json!({"role":"user","content":conversation.messages[0].text}),
            json!({"role":"user","content":"another"}),
        ];
        media
            .attach_tracked(&prompt, &mut wire, LiveDialect::OpenAiChat)
            .unwrap();
        assert_eq!(
            wire[0],
            json!({"role":"system","content":"protected system instruction"})
        );
        assert_eq!(wire[1]["role"], "user");
        assert_eq!(wire[1]["content"][0]["text"], "question");
        assert_eq!(wire[1]["content"][1]["type"], "image_url");
        assert_eq!(wire[2]["content"], "another");

        let mut malformed = vec![
            json!({"role":"system","content":"protected system instruction"}),
            json!({"role":"system","content":conversation.messages[0].text}),
            json!({"role":"user","content":"another"}),
        ];
        let before = malformed.clone();
        assert!(
            media
                .attach_tracked(&prompt, &mut malformed, LiveDialect::OpenAiChat)
                .is_err()
        );
        assert_eq!(malformed, before);
        fs::remove_dir_all(path).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn serializing_two_media_messages_never_partially_mutates_invalid_wire_payload() {
        let first = EventId::new();
        let second = EventId::new();
        let media = LiveMedia {
            entries: HashMap::from([
                (
                    first,
                    MediaEntry {
                        role: PromptRole::User,
                        projected_text: "first".into(),
                        parts: vec![json!({"text":"first"})],
                    },
                ),
                (
                    second,
                    MediaEntry {
                        role: PromptRole::Assistant,
                        projected_text: "second".into(),
                        parts: vec![json!({"text":"second"})],
                    },
                ),
            ]),
        };
        let prompt = [
            (PromptRole::System, "instruction", None),
            (PromptRole::User, "first", Some(first)),
            (PromptRole::Assistant, "second", Some(second)),
        ]
        .into_iter()
        .map(|(role, text, source)| {
            super::super::transforms::TrackedPrompt::new(
                PromptMessage {
                    role,
                    text: text.into(),
                },
                source,
            )
        })
        .collect::<Vec<_>>();
        let mut wire = vec![
            json!({"role":"system","content":"instruction"}),
            json!({"role":"user","content":"first"}),
            json!({"role":"user","content":"second"}),
        ];
        let initial = wire.clone();
        assert!(
            media
                .attach_tracked(&prompt, &mut wire, LiveDialect::OpenAiChat)
                .is_err()
        );
        assert_eq!(wire, initial);
    }

    #[test]
    fn historical_visibility_hide_reset_and_fork_do_not_rebind_media() {
        let root = std::env::temp_dir().join(format!("nm-media-history-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let image = assets
            .import("image/png", Cursor::new(b"secret-image"))
            .unwrap();
        let mut repository = TimelineStore::new();
        let branch = BranchId::new();
        repository.create_session(SessionId::new(), branch).unwrap();
        let mut draft = EventDraft::new(EventKind::PlayerMessage);
        draft.observers.insert(CharacterId::Crazy);
        draft.actor = Some("player".into());
        draft
            .payload
            .insert("content".into(), json!([{"type":"image","asset":image}]));
        let id = draft.id;
        repository
            .append(branch, 0, "origin", json!({}), vec![draft])
            .unwrap();
        let historic =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Crazy, Some(1))
                .unwrap();
        assert!(
            LiveMedia::prepare(
                &conversation(&historic),
                &historic,
                Some(&assets),
                LiveDialect::Gemini
            )
            .unwrap()
            .is_some()
        );
        let outsider =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Kind, None).unwrap();
        assert!(
            LiveMedia::prepare(
                &conversation(&outsider),
                &outsider,
                Some(&assets),
                LiveDialect::Gemini
            )
            .unwrap()
            .is_none()
        );

        let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
        checkpoint.payload.insert("snapshot".into(), json!({}));
        let checkpoint_id = checkpoint.id;
        repository
            .append(branch, 1, "checkpoint", json!({}), vec![checkpoint])
            .unwrap();
        let fork = BranchId::new();
        repository.fork(branch, checkpoint_id, fork).unwrap();

        let mut hidden = EventDraft::new(EventKind::MessageHidden);
        hidden.observers.insert(CharacterId::Crazy);
        hidden
            .payload
            .insert("event_ids".into(), json!([id.0.to_string()]));
        repository
            .append(branch, 2, "hide", json!({}), vec![hidden])
            .unwrap();
        let hidden_view =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Crazy, None).unwrap();
        assert!(
            LiveMedia::prepare(
                &conversation(&hidden_view),
                &hidden_view,
                Some(&assets),
                LiveDialect::OpenAiChat
            )
            .unwrap()
            .is_none()
        );
        let fork_view =
            TimelineMessageQueries::capture(&repository, fork, CharacterId::Crazy, None).unwrap();
        assert!(
            LiveMedia::prepare(
                &conversation(&fork_view),
                &fork_view,
                Some(&assets),
                LiveDialect::OpenAiChat
            )
            .unwrap()
            .is_some()
        );

        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        reset.payload.insert("reason".into(), json!("fixture"));
        repository
            .append(branch, 3, "reset", json!({}), vec![reset])
            .unwrap();
        let after_reset =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Crazy, None).unwrap();
        assert!(
            LiveMedia::prepare(
                &conversation(&after_reset),
                &after_reset,
                Some(&assets),
                LiveDialect::Gemini
            )
            .unwrap()
            .is_none()
        );
        assert!(
            LiveMedia::prepare(
                &conversation(&historic),
                &historic,
                Some(&assets),
                LiveDialect::Gemini
            )
            .unwrap()
            .is_some()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unselected_broken_or_unsupported_assets_do_not_block_selected_media() {
        let root = std::env::temp_dir().join(format!("nm-media-select-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let missing = assets
            .import("image/png", Cursor::new(b"unrelated"))
            .unwrap();
        let unsupported = assets
            .import("audio/ogg", Cursor::new(b"not-an-openai-image"))
            .unwrap();
        let selected_image = assets
            .import("image/png", Cursor::new(b"selected"))
            .unwrap();
        let missing_path = root
            .join("assets/objects")
            .join(&missing.digest[..2])
            .join(&missing.digest);
        fs::remove_file(missing_path).unwrap();
        let mut repo = TimelineStore::new();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        for content in [
            json!([{"type":"image", "asset":missing}]),
            json!([{"type":"audio", "asset":unsupported}]),
            json!([{"type":"image", "asset":selected_image}]),
        ] {
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.observers.insert(CharacterId::Crazy);
            draft.actor = Some("player".into());
            draft.payload.insert("content".into(), content);
            let head = repo.branch(branch).unwrap().head_sequence;
            repo.append(
                branch,
                head,
                &Uuid::new_v4().to_string(),
                json!({}),
                vec![draft],
            )
            .unwrap();
        }
        let view =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
        let mut selected_conversation = conversation(&view);
        let selected_event = selected_conversation.messages.last().unwrap().source;
        selected_conversation
            .messages
            .retain(|message| message.source == selected_event);
        for dialect in [
            LiveDialect::Gemini,
            LiveDialect::OpenAiChat,
            LiveDialect::OpenAiResponses,
        ] {
            let media = LiveMedia::prepare(&selected_conversation, &view, Some(&assets), dialect)
                .unwrap()
                .unwrap();
            assert_eq!(media.entries.len(), 1);
            assert!(media.entries.contains_key(&selected_event.unwrap()));
        }
        assert!(super::super::multimodal::gemini_contents(&view, &assets).is_err());
        assert!(
            super::super::multimodal::openai_contents(
                &view,
                &assets,
                super::super::multimodal::OpenAiImageDialect::ChatCompletions,
            )
            .is_err()
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unselected_invalid_part_is_not_parsed_during_live_generation() {
        let root = std::env::temp_dir().join(format!("nm-media-invalid-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let image = assets
            .import("image/png", Cursor::new(b"selected"))
            .unwrap();
        let mut repo = TimelineStore::new();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        for content in [
            json!([{"type":"unknown-media", "data":"not a provider part"}]),
            json!([{"type":"image", "asset":image}]),
        ] {
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.observers.insert(CharacterId::Crazy);
            draft.actor = Some("player".into());
            draft.payload.insert("content".into(), content);
            let head = repo.branch(branch).unwrap().head_sequence;
            repo.append(
                branch,
                head,
                &Uuid::new_v4().to_string(),
                json!({}),
                vec![draft],
            )
            .unwrap();
        }
        let view =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
        let selected_conversation = conversation(&view);
        assert_eq!(selected_conversation.messages.len(), 1);
        assert!(
            LiveMedia::prepare(
                &selected_conversation,
                &view,
                Some(&assets),
                LiveDialect::OpenAiChat
            )
            .unwrap()
            .is_some()
        );
        assert!(super::super::multimodal::visible_multimodal_messages(&view).is_err());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn edited_media_is_scoped_to_its_historical_view() {
        let root = std::env::temp_dir().join(format!("nm-media-edits-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let original = assets.import("image/png", Cursor::new(b"before")).unwrap();
        let revised = assets.import("image/png", Cursor::new(b"after")).unwrap();
        let mut repo = TimelineStore::new();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut initial = EventDraft::new(EventKind::PlayerMessage);
        initial.observers.insert(CharacterId::Crazy);
        initial.actor = Some("player".into());
        initial.payload.insert(
            "content".into(),
            json!([{"type":"image", "asset":original}]),
        );
        let id = initial.id;
        repo.append(branch, 0, "original", json!({}), vec![initial])
            .unwrap();
        let historic =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, Some(1)).unwrap();
        let mut edit = EventDraft::new(EventKind::MessageRevised);
        edit.observers.insert(CharacterId::Crazy);
        edit.payload
            .insert("event_id".into(), json!(id.0.to_string()));
        edit.payload.insert(
            "message".into(),
            json!({"content":[{"type":"image", "asset":revised}]}),
        );
        repo.append(branch, 1, "revision", json!({}), vec![edit])
            .unwrap();
        let current =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
        assert!(
            LiveMedia::prepare(
                &conversation(&historic),
                &current,
                Some(&assets),
                LiveDialect::OpenAiChat
            )
            .is_err()
        );
        assert!(
            LiveMedia::prepare(
                &conversation(&current),
                &historic,
                Some(&assets),
                LiveDialect::Gemini
            )
            .is_err()
        );
        for dialect in [
            LiveDialect::Gemini,
            LiveDialect::OpenAiChat,
            LiveDialect::OpenAiResponses,
        ] {
            let historical_media =
                LiveMedia::prepare(&conversation(&historic), &historic, Some(&assets), dialect)
                    .unwrap()
                    .unwrap();
            let current_media =
                LiveMedia::prepare(&conversation(&current), &current, Some(&assets), dialect)
                    .unwrap()
                    .unwrap();
            let old = historical_media.entries.get(&id).unwrap();
            let new = current_media.entries.get(&id).unwrap();
            assert_ne!(old.parts, new.parts);
            assert!(
                old.parts
                    .iter()
                    .any(|part| part.to_string().contains("YmVmb3Jl"))
            );
            assert!(
                new.parts
                    .iter()
                    .any(|part| part.to_string().contains("YWZ0ZXI="))
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn corrupt_or_missing_asset_is_rejected_before_provider_serialization() {
        let root = std::env::temp_dir().join(format!("nm-media-corruption-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let image = assets
            .import("image/png", Cursor::new(b"sensitive"))
            .unwrap();
        let object = root
            .join("assets/objects")
            .join(&image.digest[..2])
            .join(&image.digest);
        let (path, _unused, view, _) = environment(json!([{"type":"image","asset":image}]));
        let conversation = conversation(&view);
        fs::write(&object, b"modified!").unwrap();
        assert!(
            LiveMedia::prepare(&conversation, &view, Some(&assets), LiveDialect::OpenAiChat)
                .is_err()
        );
        fs::remove_file(&object).unwrap();
        assert!(
            LiveMedia::prepare(&conversation, &view, Some(&assets), LiveDialect::Gemini).is_err()
        );
        fs::remove_dir_all(path).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unsupported_media_is_error_not_text_fallback() {
        let root = std::env::temp_dir().join(format!("nm-media-unsupported-{}", Uuid::new_v4()));
        let store = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let asset = store.import("audio/ogg", Cursor::new(b"voice")).unwrap();
        let (path, _unused, view, _) = environment(json!([{"type":"audio","asset":asset}]));
        let c = conversation(&view);
        assert!(LiveMedia::prepare(&c, &view, Some(&store), LiveDialect::OpenAiChat).is_err());
        assert!(
            LiveMedia::prepare(&c, &view, Some(&store), LiveDialect::Gemini)
                .unwrap()
                .is_some()
        );
        fs::remove_dir_all(path).ok();
        fs::remove_dir_all(root).ok();
    }
}

#[cfg(test)]
mod live_http_tests {
    use super::*;
    use crate::{
        character::{CharacterContext, CharacterState, GenerationFocus},
        generation::{
            GenerationObserver, GenerationPreset, GenerationRequest, PreparedRequest,
            ProviderTemplate, ResponseGenerator,
        },
        generation::{
            gemini::{GeminiProvider, GeminiTemplate, GenerationSettings},
            openai::{OpenAiPresetDefinition, OpenAiProtocol, OpenAiProvider, OpenAiTemplate},
        },
        identity::CharacterId,
        interaction::TurnIntent,
        timeline::{
            ContextAnchor, HistoryView,
            domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
            selection::{TimelineSelection, TimelineTurn},
            view::TimelineMessageQueries,
        },
    };
    use std::{io::Cursor, sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
    };
    use uuid::Uuid;

    #[derive(Default)]
    struct Observe(Vec<PreparedRequest>);
    impl GenerationObserver for Observe {
        fn request_prepared(&mut self, request: PreparedRequest) {
            self.0.push(request);
        }
    }

    fn context(view: MitaView, branch: BranchId, target: EventId) -> CharacterContext {
        CharacterContext {
            variables: Default::default(),
            memory_relations: Vec::new(),
            character_id: CharacterId::Crazy,
            description: Arc::from("Mita"),
            prompt: Arc::from("Return structured JSON"),
            runtime_context: None,
            state: CharacterState {
                accepted_replies: 0,
                revision: 0,
                attitude: 60.0,
                boredom: 10.0,
                stress: 5.0,
            },
            working_state: Default::default(),
            memories: Vec::new(),
            memory_islands: Vec::new(),
            reminders: Vec::new(),
            history: HistoryView {
                at: ContextAnchor {
                    branch_id: branch.0,
                    through: 1,
                },
                events: vec![],
            },
            view,
            current_input: Arc::from(""),
            focus: GenerationFocus {
                intent: TurnIntent::Reply(target),
                reaction: None,
            },
            anchor: ContextAnchor {
                branch_id: branch.0,
                through: 1,
            },
            turn: TimelineTurn {
                selection: TimelineSelection {
                    branch_id: branch,
                    generation: 1,
                },
                context_head: 1,
                character: CharacterId::Crazy,
                context_epoch: 0,
                trigger: Some(target),
            },
            game: None,
            game_key: None,
            game_guard: None,
        }
    }

    fn provider(
        provider_name: &str,
        endpoint: &str,
    ) -> (Arc<dyn super::super::GenerationProvider>, GenerationPreset) {
        match provider_name {
            "gemini" => {
                let template = GeminiTemplate::new(
                    endpoint,
                    "fixture-model",
                    "test-key",
                    GenerationSettings::new(0.5, 128, Duration::from_secs(3)).unwrap(),
                )
                .unwrap();
                (
                    Arc::new(GeminiProvider::new().unwrap()),
                    GenerationPreset::new(ProviderTemplate::Gemini(template)),
                )
            }
            _ => {
                let protocol = match provider_name {
                    "responses" => OpenAiProtocol::Responses,
                    "mistral" => OpenAiProtocol::Mistral,
                    _ => OpenAiProtocol::Compatible,
                };
                let template = OpenAiTemplate::new(
                    OpenAiPresetDefinition {
                        protocol,
                        endpoint: endpoint.into(),
                        model: "fixture-model".into(),
                        credential: None,
                        temperature: 0.5,
                        max_output_tokens: 128,
                        timeout_ms: 3000,
                        options: protocol.options(),
                        transforms: protocol.transforms(),
                    },
                    None,
                )
                .unwrap();
                (
                    Arc::new(OpenAiProvider::new().unwrap()),
                    GenerationPreset::new(ProviderTemplate::OpenAi(template)),
                )
            }
        }
    }

    #[tokio::test]
    async fn live_provider_payloads_contain_image_once_and_observer_never_sees_binary() {
        for provider_name in ["gemini", "chat", "responses"] {
            let directory = std::env::temp_dir().join(format!("nm-live-wire-{}", Uuid::new_v4()));
            let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
            let image = assets
                .import("image/png", Cursor::new(b"secret-binary-image"))
                .unwrap();
            let mut store = TimelineStore::new();
            let branch = BranchId::new();
            store.create_session(SessionId::new(), branch).unwrap();
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft.payload.insert("content".into(),json!([{"type":"text","text":"look"},{"type":"image","asset":image},{"type":"text","text":"again"}]));
            let target = draft.id;
            store
                .append(branch, 0, "image-fixture", json!({}), vec![draft])
                .unwrap();
            let view =
                TimelineMessageQueries::capture(&store, branch, CharacterId::Crazy, None).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let name = provider_name.to_owned();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse::<usize>().unwrap();
                    }
                }
                let mut data = vec![0; length];
                reader.read_exact(&mut data).await.unwrap();
                let payload: Value = serde_json::from_slice(&data).unwrap();
                let answer = "{\"segments\":[{\"text\":\"ack\"}]}";
                let reply = if name == "gemini" {
                    json!({"candidates":[{"content":{"role":"model","parts":[{"text":answer}]},"finishReason":"STOP"}]})
                } else if name == "responses" {
                    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":answer}]}]})
                } else {
                    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":answer}}]})
                };
                let reply = reply.to_string();
                reader.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.unwrap();
                payload
            });
            let (provider, preset) = provider(provider_name, &endpoint);
            let generator = ResponseGenerator::new([provider]).unwrap();
            let mut observation = Observe::default();
            let mut request_context = context(view, branch, target);
            request_context.current_input = Arc::from("look\nagain");
            let output = generator
                .generate_observed(
                    GenerationRequest::new(request_context, preset, None)
                        .with_assets(Some(Arc::clone(&assets))),
                    &mut observation,
                )
                .await
                .unwrap();
            assert_eq!(output.text().as_ref(), "ack");
            let payload = server.await.unwrap();
            let entries = if provider_name == "gemini" {
                &payload["contents"]
            } else if provider_name == "responses" {
                &payload["input"]
            } else {
                &payload["messages"]
            };
            let serialized = entries.to_string();
            assert_eq!(
                serialized.matches("c2VjcmV0LWJpbmFyeS1pbWFnZQ==").count(),
                1,
                "{provider_name}"
            );
            assert_eq!(serialized.matches("look").count(), 1, "{provider_name}");
            assert_eq!(serialized.matches("again").count(), 1, "{provider_name}");
            let actual_messages = entries.as_array().unwrap();
            let media_index = actual_messages
                .iter()
                .position(|entry| entry.to_string().contains("c2VjcmV0LWJpbmFyeS1pbWFnZQ=="))
                .unwrap();
            assert_eq!(
                actual_messages[media_index]["role"], "user",
                "{provider_name}"
            );
            assert!(
                actual_messages.iter().enumerate().all(|(index, entry)| {
                    index == media_index
                        || !entry.to_string().contains("c2VjcmV0LWJpbmFyeS1pbWFnZQ==")
                }),
                "{provider_name}"
            );
            if provider_name != "gemini" {
                assert_eq!(actual_messages[0]["role"], "system");
                assert!(media_index > 0, "user media replaced system instruction");
            }
            assert_eq!(observation.0.len(), 1);
            assert_eq!(
                observation.0[0].payload_json.as_ref(),
                "[REDACTED MULTIMODAL ATTACHMENTS]"
            );
            std::fs::remove_dir_all(directory).ok();
        }
    }
    #[tokio::test]
    async fn missing_or_corrupt_binary_never_reaches_provider_or_observer() {
        for provider_name in ["gemini", "chat", "responses"] {
            for corrupt in [true, false] {
                let directory =
                    std::env::temp_dir().join(format!("nm-media-reject-{}", Uuid::new_v4()));
                let assets =
                    Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
                let image = assets
                    .import("image/png", Cursor::new(b"sensitive"))
                    .unwrap();
                let object = directory
                    .join("assets/objects")
                    .join(&image.digest[..2])
                    .join(&image.digest);
                let mut store = TimelineStore::new();
                let branch = BranchId::new();
                store.create_session(SessionId::new(), branch).unwrap();
                let mut draft = EventDraft::new(EventKind::PlayerMessage);
                draft.actor = Some("player".into());
                draft.observers.insert(CharacterId::Crazy);
                draft.payload.insert(
                    "content".into(),
                    json!([{"type":"text","text":"inspect"},{"type":"image","asset":image}]),
                );
                let target = draft.id;
                store
                    .append(branch, 0, "fixture", json!({}), vec![draft])
                    .unwrap();
                let view =
                    TimelineMessageQueries::capture(&store, branch, CharacterId::Crazy, None)
                        .unwrap();

                if corrupt {
                    std::fs::write(&object, b"tampered!").unwrap();
                } else {
                    std::fs::remove_file(&object).unwrap();
                }
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}/", listener.local_addr().unwrap());
                let (adapter, preset) = provider(provider_name, &endpoint);
                let generator = ResponseGenerator::new([adapter]).unwrap();
                let mut observation = Observe::default();
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    generator.generate_observed(
                        GenerationRequest::new(context(view, branch, target), preset, None)
                            .with_assets(Some(Arc::clone(&assets))),
                        &mut observation,
                    ),
                )
                .await;
                assert!(
                    matches!(result, Ok(Err(GenerationError::Response(_)))),
                    "{provider_name} corrupt={corrupt}: {result:?}"
                );
                assert!(observation.0.is_empty(), "{provider_name}");
                drop(listener);
                std::fs::remove_dir_all(directory).ok();
            }
        }
    }
    #[tokio::test]
    async fn merging_two_user_events_preserves_both_media_and_event_order() {
        let directory = std::env::temp_dir().join(format!("nm-media-merge-{}", Uuid::new_v4()));
        let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
        let first_image = assets
            .import("image/png", Cursor::new(b"first-binary"))
            .unwrap();
        let second_image = assets
            .import("image/png", Cursor::new(b"second-binary"))
            .unwrap();
        let mut repository = TimelineStore::new();
        let branch = BranchId::new();
        repository.create_session(SessionId::new(), branch).unwrap();
        let mut drafts = Vec::new();
        for (text, asset) in [("repeat", first_image), ("repeat", second_image)] {
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft.payload.insert(
                "content".into(),
                json!([{"type":"text","text":text},{"type":"image","asset":asset}]),
            );
            drafts.push(draft);
        }
        let target = drafts[1].id;
        repository
            .append(branch, 0, "fixture", json!({}), drafts)
            .unwrap();
        let view =
            TimelineMessageQueries::capture(&repository, branch, CharacterId::Crazy, None).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(raw) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = raw.trim().parse::<usize>().unwrap();
                }
            }
            let mut data = vec![0; length];
            reader.read_exact(&mut data).await.unwrap();
            let body: Value = serde_json::from_slice(&data).unwrap();
            let response = json!({"choices":[{"finish_reason":"stop","message":{
                "role":"assistant","content":"{\"segments\":[{\"text\":\"ack\"}]}"
            }}]})
            .to_string();
            reader.get_mut().write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()
            ).as_bytes()).await.unwrap();
            body
        });
        let (adapter, preset) = provider("mistral", &endpoint);
        let generator = ResponseGenerator::new([adapter]).unwrap();
        let mut observation = Observe::default();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            generator.generate_observed(
                GenerationRequest::new(context(view, branch, target), preset, None)
                    .with_assets(Some(Arc::clone(&assets))),
                &mut observation,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.text().as_ref(), "ack");
        let body = server.await.unwrap();
        let messages = body["messages"].as_array().unwrap();
        let media = messages
            .iter()
            .filter(|message| {
                message.to_string().contains("Zmlyc3QtYmluYXJ5")
                    || message.to_string().contains("c2Vjb25kLWJpbmFyeQ==")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            media.len(),
            1,
            "both source events must stay in one user turn"
        );
        assert_eq!(media[0]["role"], "user");
        let serialized = media[0]["content"].to_string();
        assert_eq!(serialized.matches("Zmlyc3QtYmluYXJ5").count(), 1);
        assert_eq!(serialized.matches("c2Vjb25kLWJpbmFyeQ==").count(), 1);
        assert!(serialized.find("Zmlyc3QtYmluYXJ5") < serialized.find("c2Vjb25kLWJpbmFyeQ=="));
        assert_eq!(observation.0.len(), 1);
        assert_eq!(
            observation.0[0].payload_json.as_ref(),
            "[REDACTED MULTIMODAL ATTACHMENTS]"
        );
        std::fs::remove_dir_all(directory).ok();
    }
}
