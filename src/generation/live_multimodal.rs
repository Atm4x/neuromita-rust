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
        if conversation.source_observer != view.observer()
            || conversation.source_anchor.branch_id != view.branch.0
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
            super::super::conversation::project_history(view, view.observer(), None);
        LlmConversation {
            source_observer: view.observer(),
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
    fn matching_branch_and_head_from_another_observer_is_rejected() {
        let root = std::env::temp_dir().join(format!("nm-media-observer-{}", Uuid::new_v4()));
        let assets = TimelineAssetStore::new(root.join("assets"), 1024).unwrap();
        let image = assets.import("image/png", Cursor::new(b"shared")).unwrap();
        let mut repo = TimelineStore::new();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut draft = EventDraft::new(EventKind::PlayerMessage);
        draft
            .observers
            .extend([CharacterId::Crazy, CharacterId::Kind]);
        draft.actor = Some("player".into());
        draft
            .payload
            .insert("content".into(), json!([{"type":"image", "asset":image}]));
        repo.append(branch, 0, "shared", json!({}), vec![draft])
            .unwrap();
        let first =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
        let second =
            TimelineMessageQueries::capture(&repo, branch, CharacterId::Kind, None).unwrap();
        assert_eq!(first.branch, second.branch);
        assert_eq!(first.through, second.through);
        assert!(
            LiveMedia::prepare(
                &conversation(&first),
                &second,
                Some(&assets),
                LiveDialect::Gemini
            )
            .is_err()
        );
        assert!(
            LiveMedia::prepare(
                &conversation(&second),
                &first,
                Some(&assets),
                LiveDialect::OpenAiChat
            )
            .is_err()
        );
        assert!(
            LiveMedia::prepare(
                &conversation(&second),
                &second,
                Some(&assets),
                LiveDialect::OpenAiChat
            )
            .unwrap()
            .is_some()
        );
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
        let through = view.through;
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
                    through,
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
                through,
            },
            turn: TimelineTurn {
                selection: TimelineSelection {
                    branch_id: branch,
                    generation: 1,
                },
                context_head: through,
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
        provider_with_stream(provider_name, endpoint, false)
    }

    fn provider_with_stream(
        provider_name: &str,
        endpoint: &str,
        stream: bool,
    ) -> (Arc<dyn super::super::GenerationProvider>, GenerationPreset) {
        match provider_name {
            "gemini" => {
                let options = super::super::gemini::GeminiRequestOptions {
                    stream,
                    max_retries: 0,
                    ..Default::default()
                };
                let template = GeminiTemplate::new(
                    endpoint,
                    "fixture-model",
                    "test-key",
                    GenerationSettings::new(0.5, 128, Duration::from_secs(3)).unwrap(),
                )
                .unwrap()
                .with_options(options)
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
                let mut options = protocol.options();
                options.stream = stream;
                options.max_retries = 0;
                let template = OpenAiTemplate::new(
                    OpenAiPresetDefinition {
                        protocol,
                        endpoint: endpoint.into(),
                        model: "fixture-model".into(),
                        credential: None,
                        temperature: 0.5,
                        max_output_tokens: 128,
                        timeout_ms: 3000,
                        options,
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

    #[derive(Default)]
    struct StreamObserve {
        requests: Vec<PreparedRequest>,
        deltas: Vec<String>,
        replies: Vec<super::super::ProviderReply>,
        failures: Vec<String>,
        stream_resets: usize,
        started_tools: Vec<String>,
        finished_tools: usize,
        first_delta: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl GenerationObserver for StreamObserve {
        fn request_prepared(&mut self, request: PreparedRequest) {
            self.requests.push(request);
        }

        fn stream_delta(&mut self, delta: &str) {
            self.deltas.push(delta.to_owned());
            if let Some(signal) = self.first_delta.take() {
                let _ = signal.send(());
            }
        }

        fn response_received(&mut self, reply: super::super::ProviderReply) {
            self.replies.push(reply);
        }

        fn failed(&mut self, error: &GenerationError) {
            self.failures.push(error.to_string());
        }

        fn stream_reset(&mut self) {
            self.stream_resets += 1;
        }

        fn tool_started(
            &mut self,
            name: &str,
            _args: &serde_json::Map<String, Value>,
            _spoken: &str,
        ) {
            self.started_tools.push(name.to_owned());
        }

        fn tool_finished(&mut self) {
            self.finished_tools += 1;
        }
    }

    #[tokio::test]
    async fn sse_empty_media_fails_before_any_provider_request_or_observation() {
        for provider_name in ["gemini", "chat", "responses"] {
            let directory = std::env::temp_dir().join(format!("nm-sse-empty-{}", Uuid::new_v4()));
            let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
            let empty = assets.import("image/png", Cursor::new([])).unwrap();
            let mut timeline = TimelineStore::new();
            let branch = BranchId::new();
            timeline.create_session(SessionId::new(), branch).unwrap();
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft
                .payload
                .insert("content".into(), json!([{"type":"image","asset":empty}]));
            let target = draft.id;
            timeline
                .append(branch, 0, "empty-fixture", json!({}), vec![draft])
                .unwrap();
            let view = TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None)
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let (provider, preset) = provider_with_stream(provider_name, &endpoint, true);
            let generator = ResponseGenerator::new([provider]).unwrap();
            let mut observation = StreamObserve::default();
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                generator.generate_observed(
                    GenerationRequest::new(context(view, branch, target), preset, None)
                        .with_assets(Some(Arc::clone(&assets))),
                    &mut observation,
                ),
            )
            .await
            .unwrap();
            assert!(
                matches!(&result, Err(GenerationError::Response(error)) if error.contains("empty")),
                "{provider_name}: {result:?}"
            );
            assert!(observation.requests.is_empty(), "{provider_name}");
            assert!(observation.replies.is_empty(), "{provider_name}");
            assert!(observation.deltas.is_empty(), "{provider_name}");
            assert!(
                tokio::time::timeout(Duration::from_millis(75), listener.accept())
                    .await
                    .is_err(),
                "{provider_name}: empty media was sent over HTTP"
            );
            std::fs::remove_dir_all(directory).ok();
        }
    }

    #[tokio::test]
    async fn sse_multimodal_provider_wires_keep_image_once_and_redact_observations() {
        use tokio::time::timeout;

        for provider_name in ["gemini", "chat", "responses"] {
            let directory = std::env::temp_dir().join(format!("nm-sse-media-{}", Uuid::new_v4()));
            let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
            let image = assets
                .import("image/png", Cursor::new(b"private-sse-pixels"))
                .unwrap();
            let outsider_image = assets
                .import("image/png", Cursor::new(b"for-kind-only"))
                .unwrap();
            let hidden_image = assets
                .import("image/png", Cursor::new(b"hidden-from-crazy"))
                .unwrap();
            let mut timeline = TimelineStore::new();
            let branch = BranchId::new();
            timeline.create_session(SessionId::new(), branch).unwrap();
            let mut outsider = EventDraft::new(EventKind::PlayerMessage);
            outsider.actor = Some("player".into());
            outsider.observers.insert(CharacterId::Kind);
            outsider.payload.insert(
                "content".into(),
                json!([{"type":"text","text":"kind-confidential"},{"type":"image","asset":outsider_image}]),
            );
            timeline
                .append(branch, 0, "private-fixture", json!({}), vec![outsider])
                .unwrap();
            let mut previously_visible = EventDraft::new(EventKind::PlayerMessage);
            previously_visible.actor = Some("player".into());
            previously_visible.observers.insert(CharacterId::Crazy);
            previously_visible.payload.insert(
                "content".into(),
                json!([{"type":"text","text":"crazy-revoked"},{"type":"image","asset":hidden_image}]),
            );
            let hidden_event = previously_visible.id;
            timeline
                .append(
                    branch,
                    1,
                    "hidden-fixture",
                    json!({}),
                    vec![previously_visible],
                )
                .unwrap();
            let mut hide = EventDraft::new(EventKind::MessageHidden);
            hide.observers.insert(CharacterId::Crazy);
            hide.payload
                .insert("event_ids".into(), json!([hidden_event.0.to_string()]));
            timeline
                .append(branch, 2, "hide-fixture", json!({}), vec![hide])
                .unwrap();
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft.payload.insert(
                "content".into(),
                json!([{"type":"text","text":"before"},{"type":"image","asset":image},{"type":"text","text":"after"}]),
            );
            let target = draft.id;
            timeline
                .append(branch, 3, "stream-fixture", json!({}), vec![draft])
                .unwrap();
            let view = TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None)
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let name = provider_name.to_owned();
            let server = tokio::spawn(async move {
                let (socket, _) = timeout(Duration::from_secs(3), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
                let mut reader = BufReader::new(socket);
                let mut length = 0;
                let mut request_line = String::new();
                reader.read_line(&mut request_line).await.unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).await.unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let events = match name.as_str() {
                    "gemini" => {
                        assert!(request_line.contains("streamGenerateContent"));
                        vec![
                            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"{\"segments\":["}]}}]}),
                            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"{\"text\":\"ack\"}]}"}]},"finishReason":"STOP"}]}),
                        ]
                    }
                    "responses" => vec![
                        json!({"type":"response.output_text.delta","delta":"{\"segments\":["}),
                        json!({"type":"response.output_text.delta","delta":"{\"text\":\"ack\"}]}"}),
                        json!({"type":"response.completed","response":{"status":"completed"}}),
                    ],
                    _ => vec![
                        json!({"choices":[{"index":0,"delta":{"content":"{\"segments\":["},"finish_reason":null}]}),
                        json!({"choices":[{"index":0,"delta":{"content":"{\"text\":\"ack\"}]}"},"finish_reason":null}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    ],
                };
                let mut response = events
                    .iter()
                    .map(|event| format!("data: {event}\r\n\r\n"))
                    .collect::<String>();
                if name == "chat" {
                    response.push_str("data: [DONE]\r\n\r\n");
                }
                reader.get_mut().write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                ).as_bytes()).await.unwrap();
                for bytes in response.as_bytes().chunks(17) {
                    reader.get_mut().write_all(bytes).await.unwrap();
                }
                body
            });
            let (provider, preset) = provider_with_stream(provider_name, &endpoint, true);
            let generator = ResponseGenerator::new([provider]).unwrap();
            let mut observation = StreamObserve::default();
            let output = timeout(
                Duration::from_secs(5),
                generator.generate_observed(
                    GenerationRequest::new(context(view, branch, target), preset, None)
                        .with_assets(Some(Arc::clone(&assets))),
                    &mut observation,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(output.text().as_ref(), "ack");
            let payload = server.await.unwrap();
            let entries = match provider_name {
                "gemini" => &payload["contents"],
                "responses" => &payload["input"],
                _ => &payload["messages"],
            };
            let actual = entries.as_array().unwrap();
            let encoded = "cHJpdmF0ZS1zc2UtcGl4ZWxz";
            let all = entries.to_string();
            assert_eq!(all.matches(encoded).count(), 1, "{provider_name}");
            assert_eq!(all.matches("before").count(), 1, "{provider_name}");
            assert_eq!(all.matches("after").count(), 1, "{provider_name}");
            assert!(!all.contains("Zm9yLWtpbmQtb25seQ=="), "{provider_name}");
            assert!(!all.contains("aGlkZGVuLWZyb20tY3Jhenk="), "{provider_name}");
            assert!(!all.contains("kind-confidential"), "{provider_name}");
            assert!(!all.contains("crazy-revoked"), "{provider_name}");
            let media = actual
                .iter()
                .filter(|item| item.to_string().contains(encoded))
                .collect::<Vec<_>>();
            assert_eq!(media.len(), 1, "{provider_name}");
            assert_eq!(media[0]["role"], "user", "{provider_name}");
            let parts = if provider_name == "gemini" {
                &media[0]["parts"]
            } else {
                &media[0]["content"]
            };
            let parts = parts.as_array().unwrap();
            assert_eq!(parts.len(), 3, "{provider_name}");
            let image_key = if provider_name == "gemini" {
                "inlineData"
            } else {
                "image_url"
            };
            assert!(parts[1].get(image_key).is_some(), "{provider_name}");
            assert_eq!(observation.requests.len(), 1, "{provider_name}");
            let observed_prompt = observation.requests[0]
                .prompt
                .iter()
                .map(|message| message.text.as_ref())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!observed_prompt.contains("kind-confidential"));
            assert!(!observed_prompt.contains("crazy-revoked"));
            assert_eq!(
                observation.requests[0].payload_json.as_ref(),
                "[REDACTED MULTIMODAL ATTACHMENTS]"
            );
            assert_eq!(
                observation.deltas.concat(),
                "{\"segments\":[{\"text\":\"ack\"}]}"
            );
            assert_eq!(observation.replies.len(), 1, "{provider_name}");
            assert!(
                observation.failures.is_empty(),
                "{provider_name}: {:?}",
                observation.failures
            );
            assert!(observation.replies[0].complete);
            for visible in observation
                .deltas
                .iter()
                .map(String::as_str)
                .chain(observation.failures.iter().map(String::as_str))
                .chain(std::iter::once(observation.replies[0].raw_body.as_ref()))
            {
                assert!(
                    !visible.contains(encoded),
                    "media leaked to observer: {provider_name}"
                );
                assert!(!visible.contains("private-sse-pixels"));
            }
            std::fs::remove_dir_all(directory).ok();
        }
    }

    #[tokio::test]
    async fn sse_native_tool_follow_up_preserves_one_media_event_per_turn() {
        use tokio::time::timeout;

        for provider_name in ["gemini", "chat", "responses"] {
            let directory = std::env::temp_dir().join(format!("nm-sse-tools-{}", Uuid::new_v4()));
            let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
            let image = assets
                .import("image/png", Cursor::new(b"original-only-media"))
                .unwrap();
            let private = assets
                .import("image/png", Cursor::new(b"outsider-media"))
                .unwrap();
            let mut timeline = TimelineStore::new();
            let branch = BranchId::new();
            timeline.create_session(SessionId::new(), branch).unwrap();
            let mut outside = EventDraft::new(EventKind::PlayerMessage);
            outside.actor = Some("player".into());
            outside.observers.insert(CharacterId::Kind);
            outside.payload.insert(
                "content".into(),
                json!([
                    {"type":"text","text":"secret-for-kind"}, {"type":"image","asset":private}
                ]),
            );
            timeline
                .append(branch, 0, "private", json!({}), vec![outside])
                .unwrap();
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft.payload.insert(
                "content".into(),
                json!([
                    {"type":"text","text":"compute"},
                    {"type":"image","asset":image},
                    {"type":"text","text":"then reply"}
                ]),
            );
            let target = draft.id;
            timeline
                .append(branch, 1, "visible", json!({}), vec![draft])
                .unwrap();
            let stable_head = timeline.branch(branch).unwrap().head_sequence;
            let view = TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None)
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let name = provider_name.to_owned();
            let server = tokio::spawn(async move {
                let mut requests = Vec::new();
                for round in 0..2 {
                    let (socket, _) = timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                    let mut reader = BufReader::new(socket);
                    let mut length = 0;
                    let mut first_line = String::new();
                    reader.read_line(&mut first_line).await.unwrap();
                    for _ in 0..64 {
                        let mut line = String::new();
                        reader.read_line(&mut line).await.unwrap();
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                    assert!(length > 0);
                    let mut bytes = vec![0; length];
                    reader.read_exact(&mut bytes).await.unwrap();
                    let body: Value = serde_json::from_slice(&bytes).unwrap();
                    requests.push(body);
                    let events: Vec<Value> = match (name.as_str(), round) {
                        ("gemini", 0) => vec![
                            json!({"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"calculator","args":{"expression":"2+3"}}}]},"finishReason":"STOP"}]}),
                        ],
                        ("gemini", _) => vec![
                            json!({"candidates":[{"content":{"role":"model","parts":[{"text":"{\"segments\":[{\"text\":\"final\"}]}"}]},"finishReason":"STOP"}]}),
                        ],
                        ("responses", 0) => vec![
                            json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"calculator","arguments":"{\"expression\":\"2+3\"}"}}),
                            json!({"type":"response.completed","response":{"status":"completed"}}),
                        ],
                        ("responses", _) => vec![
                            json!({"type":"response.output_text.delta","delta":"{\"segments\":[{\"text\":\"final\"}]}"}),
                            json!({"type":"response.completed","response":{"status":"completed"}}),
                        ],
                        ("chat", 0) => vec![
                            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\"2+3\"}"}}]},"finish_reason":"tool_calls"}]}),
                        ],
                        ("chat", _) => vec![
                            json!({"choices":[{"index":0,"delta":{"content":"{\"segments\":[{\"text\":\"final\"}]}"},"finish_reason":"stop"}]}),
                        ],
                        _ => unreachable!(),
                    };
                    let mut response = events
                        .iter()
                        .map(|e| format!("data: {e}\r\n\r\n"))
                        .collect::<String>();
                    if name == "chat" {
                        response.push_str("data: [DONE]\r\n\r\n");
                    }
                    reader.get_mut().write_all(format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()
                    ).as_bytes()).await.unwrap();
                    for chunk in response.as_bytes().chunks(13) {
                        reader.get_mut().write_all(chunk).await.unwrap();
                    }
                }
                requests
            });
            let (provider, preset) = provider_with_stream(provider_name, &endpoint, true);
            let generator = ResponseGenerator::new([provider]).unwrap();
            let tools = super::super::ToolSettings {
                enabled: true,
                calculator: true,
                max_depth: 1,
                ..Default::default()
            };
            let mut observation = StreamObserve::default();
            let result = timeout(
                Duration::from_secs(5),
                generator.generate_observed(
                    GenerationRequest::new(context(view, branch, target), preset, None)
                        .with_tools(tools)
                        .with_assets(Some(Arc::clone(&assets))),
                    &mut observation,
                ),
            )
            .await
            .unwrap();
            assert!(result.is_ok(), "{provider_name}: {result:?}");
            assert_eq!(result.unwrap().text().as_ref(), "final", "{provider_name}");
            assert_eq!(timeline.branch(branch).unwrap().head_sequence, stable_head);
            let bodies = server.await.unwrap();
            assert_eq!(bodies.len(), 2);
            for body in &bodies {
                let serialized = body.to_string();
                assert_eq!(
                    serialized.matches("b3JpZ2luYWwtb25seS1tZWRpYQ==").count(),
                    1,
                    "{provider_name}"
                );
                assert!(
                    !serialized.contains("b3V0c2lkZXItbWVkaWE="),
                    "{provider_name}"
                );
                assert!(!serialized.contains("secret-for-kind"), "{provider_name}");
                assert_eq!(serialized.matches("compute").count(), 1, "{provider_name}");
                assert_eq!(
                    serialized.matches("then reply").count(),
                    1,
                    "{provider_name}"
                );
            }
            let second = &bodies[1];
            let item =
                match provider_name {
                    "gemini" => second["contents"].as_array().unwrap().last().unwrap()["parts"][0]
                        ["functionResponse"]["response"]["result"]
                        .as_str(),
                    "responses" => {
                        second["input"].as_array().unwrap().last().unwrap()["output"].as_str()
                    }
                    _ => second["messages"].as_array().unwrap().last().unwrap()["content"].as_str(),
                }
                .expect("native result must be serialized on next provider turn");
            assert!(item.contains("\n5\n"), "{provider_name}: {item}");
            assert_eq!(observation.requests.len(), 2, "{provider_name}");
            assert!(
                observation
                    .requests
                    .iter()
                    .all(|r| r.payload_json.as_ref() == "[REDACTED MULTIMODAL ATTACHMENTS]")
            );
            assert_eq!(observation.replies.len(), 2, "{provider_name}");
            assert_eq!(observation.stream_resets, 1, "{provider_name}");
            assert_eq!(observation.started_tools, ["calculator"], "{provider_name}");
            assert_eq!(observation.finished_tools, 1, "{provider_name}");
            assert!(
                observation.failures.is_empty(),
                "{provider_name}: {:?}",
                observation.failures
            );
            assert_eq!(
                observation.deltas.concat(),
                "{\"segments\":[{\"text\":\"final\"}]}"
            );
            std::fs::remove_dir_all(directory).ok();
        }
    }

    #[tokio::test]
    async fn cancelling_partial_sse_with_media_drops_provider_future_and_socket() {
        use tokio::time::timeout;
        use tokio_util::sync::CancellationToken;

        for name in ["gemini", "chat", "responses"] {
            let directory = std::env::temp_dir().join(format!("nm-sse-abort-{}", Uuid::new_v4()));
            let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
            let image = assets
                .import("image/png", Cursor::new(b"cancelled-pixels"))
                .unwrap();
            let mut timeline = TimelineStore::new();
            let branch = BranchId::new();
            timeline.create_session(SessionId::new(), branch).unwrap();
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft.payload.insert(
                "content".into(),
                json!([
                    {"type":"text","text":"abort after first partial answer"},
                    {"type":"image","asset":image}
                ]),
            );
            let target = draft.id;
            timeline
                .append(branch, 0, "abort-input", json!({}), vec![draft])
                .unwrap();
            let stable_head = timeline.branch(branch).unwrap().head_sequence;
            let view = TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None)
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let dialect = name.to_owned();
            let server = tokio::spawn(async move {
                let (socket, _) = timeout(Duration::from_secs(3), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
                let mut reader = BufReader::new(socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).await.unwrap();
                let json: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    json.to_string().matches("Y2FuY2VsbGVkLXBpeGVscw==").count(),
                    1
                );
                let event = match dialect.as_str() {
                    "gemini" => {
                        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"{\"segments\":["}]}}]})
                    }
                    "responses" => {
                        json!({"type":"response.output_text.delta","delta":"{\"segments\":["})
                    }
                    _ => {
                        json!({"choices":[{"index":0,"delta":{"content":"{\"segments\":["},"finish_reason":null}]})
                    }
                };
                let response = format!("data: {event}\r\n\r\n");
                reader.get_mut().write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 8192\r\nConnection: close\r\n\r\n"
                ).await.unwrap();
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .await
                    .unwrap();
                let mut rest = [0u8; 1];
                let closed = timeout(Duration::from_secs(3), reader.read(&mut rest)).await;
                assert!(
                    matches!(closed, Ok(Ok(0))),
                    "cancelled transport must close the socket: {closed:?}"
                );
            });
            let (provider, preset) = provider_with_stream(name, &endpoint, true);
            let generator = ResponseGenerator::new([provider]).unwrap();
            let cancellation = CancellationToken::new();
            let cancellation_task = cancellation.clone();
            let (signal, heard) = tokio::sync::oneshot::channel();
            let generation = tokio::spawn(async move {
                let mut observation = StreamObserve {
                    first_delta: Some(signal),
                    ..Default::default()
                };
                let was_cancelled = tokio::select! {
                    biased;
                    _ = cancellation_task.cancelled() => true,
                    response = generator.generate_observed(
                        GenerationRequest::new(context(view, branch, target), preset, None)
                            .with_assets(Some(assets)),
                        &mut observation,
                    ) => panic!("unexpected pre-cancel completion: {response:?}"),
                };
                (was_cancelled, observation)
            });
            timeout(Duration::from_secs(3), heard)
                .await
                .unwrap()
                .unwrap();
            cancellation.cancel();
            let (cancelled, observed) = timeout(Duration::from_secs(3), generation)
                .await
                .unwrap()
                .unwrap();
            assert!(cancelled);
            assert_eq!(observed.requests.len(), 1, "{name}");
            assert_eq!(observed.deltas, ["{\"segments\":["], "{name}");
            assert!(observed.replies.is_empty(), "{name}");
            assert!(observed.failures.is_empty(), "{name}");
            assert_eq!(timeline.branch(branch).unwrap().head_sequence, stable_head);
            timeout(Duration::from_secs(4), server)
                .await
                .unwrap()
                .unwrap();
            std::fs::remove_dir_all(directory).ok();
        }
    }
    async fn capture_history_sse_body(
        provider_name: &str,
        view: MitaView,
        branch: BranchId,
        target: EventId,
        assets: Arc<TimelineAssetStore>,
    ) -> Value {
        use tokio::time::timeout;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let dialect = provider_name.to_owned();
        let server = tokio::spawn(async move {
            let (socket, _) = timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let events = match dialect.as_str() {
                "gemini" => vec![
                    json!({"candidates":[{"content":{"role":"model","parts":[{"text":"{\"segments\":[{\"text\":\"ack\"}]}"}]},"finishReason":"STOP"}]}),
                ],
                "responses" => vec![
                    json!({"type":"response.output_text.delta","delta":"{\"segments\":[{\"text\":\"ack\"}]}"}),
                    json!({"type":"response.completed","response":{"status":"completed"}}),
                ],
                _ => vec![
                    json!({"choices":[{"index":0,"delta":{"content":"{\"segments\":[{\"text\":\"ack\"}]}"},"finish_reason":"stop"}]}),
                ],
            };
            let mut response = events
                .iter()
                .map(|event| format!("data: {event}\r\n\r\n"))
                .collect::<String>();
            if dialect == "chat" {
                response.push_str("data: [DONE]\r\n\r\n");
            }
            reader.get_mut().write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            ).as_bytes()).await.unwrap();
            for chunk in response.as_bytes().chunks(13) {
                reader.get_mut().write_all(chunk).await.unwrap();
            }
            body
        });
        let (provider, preset) = provider_with_stream(provider_name, &endpoint, true);
        let generator = ResponseGenerator::new([provider]).unwrap();
        let mut observation = StreamObserve::default();
        let reply = timeout(
            Duration::from_secs(5),
            generator.generate_observed(
                GenerationRequest::new(context(view, branch, target), preset, None)
                    .with_assets(Some(assets)),
                &mut observation,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reply.text().as_ref(), "ack", "{provider_name}");
        assert_eq!(observation.requests.len(), 1, "{provider_name}");
        assert!(observation.failures.is_empty(), "{provider_name}");
        assert_eq!(observation.replies.len(), 1, "{provider_name}");
        let body = timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
        let wire = body.to_string();
        if wire.contains("aGlzdG9yeS1") {
            assert_eq!(
                observation.requests[0].payload_json.as_ref(),
                "[REDACTED MULTIMODAL ATTACHMENTS]",
                "{provider_name}: observer must redact media-bearing bodies"
            );
        }
        assert!(
            observation.requests[0]
                .prompt
                .iter()
                .all(|message| !message.text.contains("other-character-confidential")),
            "{provider_name}: private text leaked to the observer prompt"
        );
        for secret in [
            "aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk=",
            "aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ==",
            "aGlzdG9yeS1raW5kLXByaXZhdGU=",
        ] {
            assert!(
                !observation.requests[0].payload_json.contains(secret),
                "{provider_name}: media bytes leaked to observer"
            );
            assert!(
                !observation.deltas.join("").contains(secret),
                "{provider_name}: media bytes leaked to streaming callback"
            );
        }
        body
    }

    #[tokio::test]
    async fn live_sse_historical_edit_hide_reset_and_fork_preserve_exact_media_snapshot() {
        let directory = std::env::temp_dir().join(format!("nm-sse-history-{}", Uuid::new_v4()));
        let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 2048).unwrap());
        let original = assets
            .import("image/png", Cursor::new(b"history-original-binary"))
            .unwrap();
        let revised = assets
            .import("image/png", Cursor::new(b"history-revised-binary"))
            .unwrap();
        let private = assets
            .import("image/png", Cursor::new(b"history-kind-private"))
            .unwrap();
        let mut timeline = TimelineStore::new();
        let main = BranchId::new();
        timeline.create_session(SessionId::new(), main).unwrap();

        let mut foreign = EventDraft::new(EventKind::PlayerMessage);
        foreign.actor = Some("player".into());
        foreign.observers.insert(CharacterId::Kind);
        foreign.payload.insert(
            "content".into(),
            json!([
                {"type":"text","text":"other-character-confidential"},
                {"type":"image","asset":private}
            ]),
        );
        timeline
            .append(main, 0, "foreign", json!({}), vec![foreign])
            .unwrap();

        let mut original_message = EventDraft::new(EventKind::PlayerMessage);
        original_message.actor = Some("player".into());
        original_message.observers.insert(CharacterId::Crazy);
        original_message.payload.insert(
            "content".into(),
            json!([
                {"type":"text","text":"original-prefix"},
                {"type":"image","asset":original},
                {"type":"text","text":"original-suffix"}
            ]),
        );
        let source_id = original_message.id;
        timeline
            .append(main, 1, "media", json!({}), vec![original_message])
            .unwrap();
        let before_edit =
            TimelineMessageQueries::capture(&timeline, main, CharacterId::Crazy, Some(2)).unwrap();

        let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
        checkpoint.payload.insert("snapshot".into(), json!({}));
        let checkpoint_id = checkpoint.id;
        timeline
            .append(main, 2, "checkpoint", json!({}), vec![checkpoint])
            .unwrap();
        let sibling = BranchId::new();
        timeline.fork(main, checkpoint_id, sibling).unwrap();
        let on_sibling =
            TimelineMessageQueries::capture(&timeline, sibling, CharacterId::Crazy, None).unwrap();

        let mut edit = EventDraft::new(EventKind::MessageRevised);
        edit.observers.insert(CharacterId::Crazy);
        edit.payload
            .insert("event_id".into(), json!(source_id.0.to_string()));
        edit.payload.insert(
            "message".into(),
            json!({"content":[
                {"type":"text","text":"revised-prefix"},
                {"type":"image","asset":revised},
                {"type":"text","text":"revised-suffix"}
            ]}),
        );
        timeline
            .append(main, 3, "revised", json!({}), vec![edit])
            .unwrap();
        let after_edit =
            TimelineMessageQueries::capture(&timeline, main, CharacterId::Crazy, None).unwrap();

        let mut hide = EventDraft::new(EventKind::MessageHidden);
        hide.observers.insert(CharacterId::Crazy);
        hide.payload
            .insert("event_ids".into(), json!([source_id.0.to_string()]));
        timeline
            .append(main, 4, "hidden", json!({}), vec![hide])
            .unwrap();
        let mut safe_reply = EventDraft::new(EventKind::PlayerMessage);
        safe_reply.actor = Some("player".into());
        safe_reply.observers.insert(CharacterId::Crazy);
        safe_reply
            .payload
            .insert("content".into(), json!("safe-new-question"));
        let safe_id = safe_reply.id;
        timeline
            .append(main, 5, "safe", json!({}), vec![safe_reply])
            .unwrap();
        let after_hide =
            TimelineMessageQueries::capture(&timeline, main, CharacterId::Crazy, None).unwrap();

        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        reset.payload.insert("reason".into(), json!("testing"));
        timeline
            .append(main, 6, "reset", json!({}), vec![reset])
            .unwrap();
        let mut after_reset_message = EventDraft::new(EventKind::PlayerMessage);
        after_reset_message.actor = Some("player".into());
        after_reset_message.observers.insert(CharacterId::Crazy);
        after_reset_message
            .payload
            .insert("content".into(), json!("post-reset-question"));
        let reset_id = after_reset_message.id;
        timeline
            .append(main, 7, "post-reset", json!({}), vec![after_reset_message])
            .unwrap();
        let after_reset =
            TimelineMessageQueries::capture(&timeline, main, CharacterId::Crazy, None).unwrap();
        let head = timeline.branch(main).unwrap().head_sequence;

        for provider_name in ["gemini", "chat", "responses"] {
            for (label, branch, target, view, expected, excluded) in [
                (
                    "historical",
                    main,
                    source_id,
                    before_edit.clone(),
                    Some("aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk="),
                    Some("aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ=="),
                ),
                (
                    "edited",
                    main,
                    source_id,
                    after_edit.clone(),
                    Some("aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ=="),
                    Some("aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk="),
                ),
                (
                    "fork sibling",
                    sibling,
                    source_id,
                    on_sibling.clone(),
                    Some("aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk="),
                    Some("aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ=="),
                ),
                ("hidden", main, safe_id, after_hide.clone(), None, None),
                ("reset", main, reset_id, after_reset.clone(), None, None),
            ] {
                let bytes = capture_history_sse_body(
                    provider_name,
                    view,
                    branch,
                    target,
                    Arc::clone(&assets),
                )
                .await;
                let serialized = bytes.to_string();
                assert_eq!(
                    serialized
                        .matches("aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk=")
                        .count()
                        + serialized
                            .matches("aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ==")
                            .count(),
                    if expected.is_some() { 1 } else { 0 },
                    "{provider_name}/{label}: media duplication or leakage"
                );
                if let Some(expected) = expected {
                    assert_eq!(
                        serialized.matches(expected).count(),
                        1,
                        "{provider_name}/{label}: selected image missing"
                    );
                }
                if let Some(excluded) = excluded {
                    assert!(
                        !serialized.contains(excluded),
                        "{provider_name}/{label}: other revision leaked"
                    );
                }
                assert!(
                    !serialized.contains("aGlzdG9yeS1raW5kLXByaXZhdGU="),
                    "{provider_name}/{label}: foreign image leaked"
                );
                assert!(
                    !serialized.contains("other-character-confidential"),
                    "{provider_name}/{label}: foreign text leaked"
                );
                if label == "edited" {
                    assert!(
                        serialized.find("revised-prefix")
                            < serialized.find("aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ==")
                    );
                    assert!(
                        serialized.find("aGlzdG9yeS1yZXZpc2VkLWJpbmFyeQ==")
                            < serialized.find("revised-suffix")
                    );
                    assert!(!serialized.contains("original-prefix"));
                }
                if label == "historical" || label == "fork sibling" {
                    assert!(
                        serialized.find("original-prefix")
                            < serialized.find("aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk=")
                    );
                    assert!(
                        serialized.find("aGlzdG9yeS1vcmlnaW5hbC1iaW5hcnk=")
                            < serialized.find("original-suffix")
                    );
                    assert!(!serialized.contains("revised-prefix"));
                }
                if label == "hidden" || label == "reset" {
                    assert!(
                        !serialized.contains("original-prefix")
                            && !serialized.contains("revised-prefix"),
                        "{provider_name}/{label}: hidden text leaked"
                    );
                }
            }
        }
        assert_eq!(timeline.branch(main).unwrap().head_sequence, head);
        std::fs::remove_dir_all(directory).ok();
    }
    #[tokio::test]
    async fn live_media_rejects_inconsistent_captured_context_before_provider_http() {
        use tokio::time::timeout;

        for provider_name in ["gemini", "chat", "responses"] {
            let directory = std::env::temp_dir().join(format!("nm-anchor-{}", Uuid::new_v4()));
            let assets = Arc::new(TimelineAssetStore::new(directory.join("assets"), 1024).unwrap());
            let image = assets
                .import("image/png", Cursor::new(b"anchor-protected"))
                .unwrap();
            let mut timeline = TimelineStore::new();
            let branch = BranchId::new();
            timeline.create_session(SessionId::new(), branch).unwrap();
            let mut draft = EventDraft::new(EventKind::PlayerMessage);
            draft.actor = Some("player".into());
            draft.observers.insert(CharacterId::Crazy);
            draft
                .payload
                .insert("content".into(), json!([{"type":"image","asset":image}]));
            let target = draft.id;
            timeline
                .append(branch, 0, "anchor-fixture", json!({}), vec![draft])
                .unwrap();
            let view = TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None)
                .unwrap();

            for mismatch in [
                "anchor-through",
                "anchor-branch",
                "turn-through",
                "turn-branch",
                "turn-character",
                "observer",
            ] {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}/", listener.local_addr().unwrap());
                let (provider, preset) = provider_with_stream(provider_name, &endpoint, true);
                let generator = ResponseGenerator::new([provider]).unwrap();
                let mut invalid = context(view.clone(), branch, target);
                match mismatch {
                    "anchor-through" => invalid.anchor.through += 1,
                    "anchor-branch" => invalid.anchor.branch_id = BranchId::new().0,
                    "turn-through" => invalid.turn.context_head += 1,
                    "turn-branch" => invalid.turn.selection.branch_id = BranchId::new(),
                    "turn-character" => invalid.turn.character = CharacterId::Kind,
                    "observer" => invalid.character_id = CharacterId::Kind,
                    _ => unreachable!(),
                }
                let mut observation = StreamObserve::default();
                let outcome = timeout(
                    Duration::from_millis(400),
                    generator.generate_observed(
                        GenerationRequest::new(invalid, preset, None)
                            .with_assets(Some(Arc::clone(&assets))),
                        &mut observation,
                    ),
                )
                .await;
                assert!(
                    matches!(outcome, Ok(Err(GenerationError::Response(_)))),
                    "{provider_name}/{mismatch}: inconsistent context must fail before HTTP: {outcome:?}"
                );
                assert!(
                    observation.requests.is_empty(),
                    "{provider_name}/{mismatch}"
                );
                assert!(observation.replies.is_empty(), "{provider_name}/{mismatch}");
                assert!(
                    timeout(Duration::from_millis(75), listener.accept())
                        .await
                        .is_err(),
                    "{provider_name}/{mismatch}: inconsistent context caused HTTP transmission"
                );
            }
            std::fs::remove_dir_all(directory).ok();
        }
    }
    #[tokio::test]
    async fn native_tool_completion_yields_to_owner_cancellation_before_followup_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::watch;

        struct ImmediateNativeTool(Arc<AtomicUsize>);
        impl super::super::GenerationProvider for ImmediateNativeTool {
            fn kind(&self) -> super::super::ProviderKind {
                super::super::ProviderKind::Gemini
            }

            fn generate(
                &self,
                _: super::super::GenerationInput,
                _: ProviderTemplate,
            ) -> super::super::ProviderFuture<'_> {
                let round = self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if round != 0 {
                        std::future::pending().await
                    }
                    Ok(super::super::StructuredResponse {
                        segments: vec![super::super::ResponseSegment::spoken("Thinking")],
                        tool_call: Some(super::super::ToolCall {
                            name: "calculator".into(),
                            args: serde_json::from_value(json!({"expression":"2+3"})).unwrap(),
                            native_origin: Some(
                                super::super::structured::NativeToolOrigin::Gemini {
                                    model_content: json!({"role":"model","parts":[]}),
                                },
                            ),
                        }),
                        ..Default::default()
                    })
                })
            }
        }

        struct OwnerInvalidation(watch::Sender<bool>);
        impl GenerationObserver for OwnerInvalidation {
            fn tool_finished(&mut self) {
                self.0.send(true).unwrap();
            }
        }

        let mut timeline = TimelineStore::new();
        let branch = BranchId::new();
        timeline.create_session(SessionId::new(), branch).unwrap();
        let mut source = EventDraft::new(EventKind::PlayerMessage);
        source.actor = Some("player".into());
        source.observers.insert(CharacterId::Crazy);
        source.payload.insert(
            "content".into(),
            json!([{"type":"text","text":"calculate"}]),
        );
        let target = source.id;
        timeline
            .append(branch, 0, "input", json!({}), vec![source])
            .unwrap();
        let view =
            TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None).unwrap();
        let (_, preset) = provider_with_stream("gemini", "http://127.0.0.1:1/", true);
        let requests = Arc::new(AtomicUsize::new(0));
        let generator =
            ResponseGenerator::new([Arc::new(ImmediateNativeTool(Arc::clone(&requests)))
                as Arc<dyn super::super::GenerationProvider>])
            .unwrap();
        let tools = super::super::ToolSettings {
            enabled: true,
            calculator: true,
            max_depth: 1,
            ..Default::default()
        };
        let (changes, mut observed) = watch::channel(false);
        let mut observer = OwnerInvalidation(changes);
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                biased;
                _ = observed.changed() => {
                    assert!(*observed.borrow_and_update());
                }
                outcome = generator.generate_observed(
                    GenerationRequest::new(context(view, branch, target), preset, None)
                        .with_tools(tools),
                    &mut observer,
                ) => panic!("invalidated request completed: {outcome:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "invalidated native tool result must not start another provider request"
        );
    }
}
