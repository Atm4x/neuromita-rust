//! Event-anchored media routing through the existing text prompt pipeline.
//! A transform may alter text-only context, but it must never silently rebind
//! media bytes to another message or role.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{
    GenerationError, PromptMessage, PromptRole,
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
    /// All projected history entries (including noncanonical context) are
    /// required to match contiguously after the provider transforms.
    history: Vec<(PromptRole, String)>,
    entries: Vec<(usize, EventId, Vec<Value>)>,
}

impl LiveMedia {
    pub(crate) fn prepare(
        conversation: &LlmConversation,
        view: &MitaView,
        assets: Option<&TimelineAssetStore>,
        dialect: LiveDialect,
    ) -> Result<Option<Self>, GenerationError> {
        let visible = super::multimodal::visible_multimodal_messages(view).map_err(|error| {
            GenerationError::Response(format!("invalid visible message: {error}"))
        })?;
        let known = visible
            .iter()
            .enumerate()
            .map(|(i, message)| (message.event_id, i))
            .collect::<HashMap<_, _>>();
        let mut affected = Vec::new();
        for (position, projected) in conversation.messages.iter().enumerate() {
            let Some(id) = projected.source else { continue };
            let Some(&index) = known.get(&id) else {
                return Err(GenerationError::Response(
                    "Timeline event is not visible in the generation snapshot".into(),
                ));
            };
            let original = &visible[index];
            if original.parts.iter().any(|part| part.asset().is_some()) {
                let raw = original
                    .parts
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
                affected.push((position, id, index, prefix.to_owned()));
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
        let encoded = match dialect {
            LiveDialect::Gemini => super::multimodal::gemini_contents(view, assets),
            LiveDialect::OpenAiChat => super::multimodal::openai_contents(
                view,
                assets,
                super::multimodal::OpenAiImageDialect::ChatCompletions,
            ),
            LiveDialect::OpenAiResponses => super::multimodal::openai_contents(
                view,
                assets,
                super::multimodal::OpenAiImageDialect::Responses,
            ),
        }
        .map_err(|error| {
            GenerationError::Response(format!("multimodal provider input rejected: {error}"))
        })?;
        let mut entries = Vec::new();
        for (position, id, index, prefix) in affected {
            let mut parts = encoded[index][if matches!(dialect, LiveDialect::Gemini) {
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
            entries.push((position, id, parts));
        }
        Ok(Some(Self {
            history: conversation
                .messages
                .iter()
                .map(|message| {
                    (
                        match message.role {
                            LlmRole::User => PromptRole::User,
                            LlmRole::Assistant => PromptRole::Assistant,
                        },
                        message.text.clone(),
                    )
                })
                .collect(),
            entries,
        }))
    }

    pub(crate) fn attach(
        &self,
        prompt: &[PromptMessage],
        contents: &mut [Value],
        dialect: LiveDialect,
    ) -> Result<(), GenerationError> {
        let non_system = prompt
            .iter()
            .filter(|message| message.role != PromptRole::System)
            .collect::<Vec<_>>();
        if contents.len() < non_system.len() || self.history.is_empty() {
            return Err(GenerationError::Response(
                "transformed prompt has invalid multimodal shape".into(),
            ));
        }
        let candidates = non_system
            .windows(self.history.len())
            .enumerate()
            .filter_map(|(index, window)| {
                window
                    .iter()
                    .zip(&self.history)
                    .all(|(message, (role, text))| {
                        message.role == *role && message.text.as_ref() == text
                    })
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let [start] = candidates.as_slice() else {
            return Err(GenerationError::Response(
                "provider transform changed or ambiguously positioned multimodal context".into(),
            ));
        };
        for (position, _id, parts) in &self.entries {
            let target = &mut contents[start + position];
            match dialect {
                LiveDialect::Gemini => target["parts"] = Value::Array(parts.clone()),
                LiveDialect::OpenAiChat | LiveDialect::OpenAiResponses => {
                    target["content"] = Value::Array(parts.clone())
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
            system_instruction: "instruction".into(),
            messages,
        }
    }

    fn prompt(conversation: &LlmConversation, leading: &str) -> Vec<PromptMessage> {
        std::iter::once(PromptMessage {
            role: PromptRole::System,
            text: leading.into(),
        })
        .chain(conversation.messages.iter().map(|message| PromptMessage {
            role: match message.role {
                LlmRole::User => PromptRole::User,
                LlmRole::Assistant => PromptRole::Assistant,
            },
            text: message.text.as_str().into(),
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
        let mut contents = vec![
            json!({"role":"user","parts":[{"text": "system"}]}),
            json!({"role":"user","parts":[{"text":c.messages[0].text}]}),
        ];
        media
            .attach(&p, &mut contents, LiveDialect::Gemini)
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
            json!({"role":"user","content":[]}),
        ];
        media
            .attach(&p, &mut contents, LiveDialect::OpenAiResponses)
            .unwrap();
        assert_eq!(contents[0]["content"][0]["type"], "input_image");
        assert!(
            contents[0]["content"][0]["image_url"]
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
        let changed = vec![PromptMessage {
            role: PromptRole::User,
            text: "changed".into(),
        }];
        assert!(
            media
                .attach(&changed, &mut contents, LiveDialect::OpenAiChat)
                .is_err()
        );
        let matching = vec![
            PromptMessage {
                role: PromptRole::User,
                text: c.messages[0].text.clone().into(),
            },
            PromptMessage {
                role: PromptRole::User,
                text: c.messages[0].text.clone().into(),
            },
        ];
        let mut contents = vec![json!({"content":"repeat"}), json!({"content":"repeat"})];
        assert!(
            media
                .attach(&matching, &mut contents, LiveDialect::OpenAiChat)
                .is_err()
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
            let (provider, preset): (Arc<dyn super::super::GenerationProvider>, GenerationPreset) =
                match provider_name {
                    "gemini" => {
                        let template = GeminiTemplate::new(
                            &endpoint,
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
                        let protocol = if provider_name == "responses" {
                            OpenAiProtocol::Responses
                        } else {
                            OpenAiProtocol::Compatible
                        };
                        let template = OpenAiTemplate::new(
                            OpenAiPresetDefinition {
                                protocol,
                                endpoint,
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
                };
            let generator = ResponseGenerator::new([provider]).unwrap();
            let mut observation = Observe::default();
            let output = generator
                .generate_observed(
                    GenerationRequest::new(context(view, branch, target), preset, None)
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
            assert!(serialized.contains("look"));
            assert!(serialized.contains("again"));
            assert_eq!(observation.0.len(), 1);
            assert_eq!(
                observation.0[0].payload_json.as_ref(),
                "[REDACTED MULTIMODAL ATTACHMENTS]"
            );
            std::fs::remove_dir_all(directory).ok();
        }
    }
}
