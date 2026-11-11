//! Provider-neutral view of ordered Timeline message parts and explicit codecs.
//! These codecs accept only a caller-supplied MitaView and injected asset store.
//! They do not select a branch, root directory, prompt role or alternate history.

use std::collections::{HashMap, HashSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    identity::EventId,
    timeline::{
        domain::{MessageRole, TimelineContentError, TimelineMessagePart, parse_message_content},
        storage::assets::{TimelineAssetError, TimelineAssetStore},
        view::MitaView,
    },
};

const MAX_PART_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleMultimodalMessage {
    pub event_id: EventId,
    pub message_id: String,
    pub external_message_id: Option<String>,
    pub role: MessageRole,
    pub speaker: Option<String>,
    pub parts: Vec<TimelineMessagePart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiImageDialect {
    ChatCompletions,
    Responses,
}

#[derive(Debug, Error)]
pub enum MultimodalError {
    #[error("invalid visible message content: {0}")]
    InvalidContent(#[from] TimelineContentError),
    #[error("Timeline attachment not available or invalid: {0}")]
    Asset(#[from] TimelineAssetError),
    #[error("provider does not support this media type or message role")]
    Unsupported,
    #[error("provider media payload exceeds the configured limit")]
    TooLarge,
    #[error("provider media payload is empty")]
    EmptyMedia,
    #[error("selected Timeline message is not visible in the generation snapshot")]
    MissingVisibleEvent,
}

/// The sole source for this projection is MitaView; hidden/revoked messages are
/// excluded before an asset identifier can be read.
pub fn visible_multimodal_messages(
    view: &MitaView,
) -> Result<Vec<VisibleMultimodalMessage>, MultimodalError> {
    visible_multimodal_messages_selected(view, None)
}

fn visible_multimodal_messages_selected(
    view: &MitaView,
    selected: Option<&HashSet<EventId>>,
) -> Result<Vec<VisibleMultimodalMessage>, MultimodalError> {
    view.messages()
        .into_iter()
        .filter(|message| selected.is_none_or(|ids| ids.contains(&message.event_id)))
        .map(|message| {
            let parts = message
                .fields
                .get("content")
                .map(parse_message_content)
                .transpose()?
                .unwrap_or_default();
            Ok(VisibleMultimodalMessage {
                event_id: message.event_id,
                message_id: message.message_id,
                external_message_id: message.external_message_id,
                role: message.role,
                speaker: message.speaker,
                parts,
            })
        })
        .collect()
}

struct MediaBudget(u64);

impl MediaBudget {
    fn read(
        &mut self,
        store: &TimelineAssetStore,
        view: &MitaView,
        event: EventId,
        reference: &crate::timeline::domain::TimelineAssetRef,
    ) -> Result<String, MultimodalError> {
        if reference.size == 0 {
            return Err(MultimodalError::EmptyMedia);
        }
        if reference.size > MAX_PART_BYTES {
            return Err(MultimodalError::TooLarge);
        }
        self.0 = self
            .0
            .checked_add(reference.size)
            .filter(|size| *size <= MAX_TOTAL_BYTES)
            .ok_or(MultimodalError::TooLarge)?;
        let bytes = store.read_visible(view, event, reference)?;
        Ok(STANDARD.encode(bytes))
    }
}

fn provider_role(role: MessageRole, assistant: &str) -> &'static str {
    match role {
        MessageRole::Assistant => {
            if assistant == "model" {
                "model"
            } else {
                "assistant"
            }
        }
        MessageRole::System | MessageRole::User => "user",
    }
}

/// Direct Gemini `generateContent.contents` representation. This is never
/// encoded as `systemInstruction`: session notes are untrusted user data.
pub fn gemini_contents(
    view: &MitaView,
    assets: &TimelineAssetStore,
) -> Result<Vec<Value>, MultimodalError> {
    Ok(gemini_content_pairs(view, assets, None)?
        .into_iter()
        .map(|(_, content)| content)
        .collect())
}

pub(crate) fn gemini_selected_contents(
    view: &MitaView,
    assets: &TimelineAssetStore,
    selected: &HashSet<EventId>,
) -> Result<HashMap<EventId, Value>, MultimodalError> {
    selected_map(
        gemini_content_pairs(view, assets, Some(selected))?,
        selected,
    )
}

fn gemini_content_pairs(
    view: &MitaView,
    assets: &TimelineAssetStore,
    selected: Option<&HashSet<EventId>>,
) -> Result<Vec<(EventId, Value)>, MultimodalError> {
    let mut budget = MediaBudget(0);
    visible_multimodal_messages_selected(view, selected)?
        .into_iter()
        .map(|message| {
            let mut parts = Vec::with_capacity(message.parts.len());
            for part in message.parts {
                let value = match part {
                    TimelineMessagePart::Text { text } => json!({"text": text}),
                    TimelineMessagePart::Image { asset } => {
                        if !matches!(
                            asset.media_type.as_str(),
                            "image/jpeg" | "image/png" | "image/webp" | "image/gif"
                        ) {
                            return Err(MultimodalError::Unsupported);
                        }
                        let data = budget.read(assets, view, message.event_id, &asset)?;
                        json!({"inlineData": {"mimeType":asset.media_type, "data":data}})
                    }
                    TimelineMessagePart::Audio { asset } => {
                        if !matches!(
                            asset.media_type.as_str(),
                            "audio/wav" | "audio/mpeg" | "audio/ogg" | "audio/flac"
                        ) {
                            return Err(MultimodalError::Unsupported);
                        }
                        let data = budget.read(assets, view, message.event_id, &asset)?;
                        json!({"inlineData": {"mimeType":asset.media_type, "data":data}})
                    }
                    TimelineMessagePart::File { .. } => return Err(MultimodalError::Unsupported),
                };
                parts.push(value);
            }
            Ok((
                message.event_id,
                json!({"role":provider_role(message.role, "model"), "parts":parts}),
            ))
        })
        .collect()
}

/// Strict OpenAI input-image mapping; unsupported audio/file content is an
/// explicit error rather than being silently discarded as text-only context.
pub fn openai_contents(
    view: &MitaView,
    assets: &TimelineAssetStore,
    dialect: OpenAiImageDialect,
) -> Result<Vec<Value>, MultimodalError> {
    Ok(openai_content_pairs(view, assets, dialect, None)?
        .into_iter()
        .map(|(_, content)| content)
        .collect())
}

pub(crate) fn openai_selected_contents(
    view: &MitaView,
    assets: &TimelineAssetStore,
    dialect: OpenAiImageDialect,
    selected: &HashSet<EventId>,
) -> Result<HashMap<EventId, Value>, MultimodalError> {
    selected_map(
        openai_content_pairs(view, assets, dialect, Some(selected))?,
        selected,
    )
}

fn selected_map(
    pairs: Vec<(EventId, Value)>,
    selected: &HashSet<EventId>,
) -> Result<HashMap<EventId, Value>, MultimodalError> {
    let mut entries = HashMap::with_capacity(pairs.len());
    for (id, content) in pairs {
        if entries.insert(id, content).is_some() {
            return Err(MultimodalError::MissingVisibleEvent);
        }
    }
    if entries.len() != selected.len() {
        return Err(MultimodalError::MissingVisibleEvent);
    }
    Ok(entries)
}

fn openai_content_pairs(
    view: &MitaView,
    assets: &TimelineAssetStore,
    dialect: OpenAiImageDialect,
    selected: Option<&HashSet<EventId>>,
) -> Result<Vec<(EventId, Value)>, MultimodalError> {
    let mut budget = MediaBudget(0);
    visible_multimodal_messages_selected(view, selected)?
        .into_iter()
        .map(|message| {
            let mut parts = Vec::with_capacity(message.parts.len());
            for part in message.parts {
                let value = match part {
                    TimelineMessagePart::Text { text } => match dialect {
                        OpenAiImageDialect::Responses => json!({"type": if message.role == MessageRole::Assistant {"output_text"} else {"input_text"}, "text":text}),
                        OpenAiImageDialect::ChatCompletions => json!({"type":"text", "text":text}),
                    },
                    TimelineMessagePart::Image { asset } => {
                        if message.role == MessageRole::Assistant || !matches!(asset.media_type.as_str(), "image/jpeg" | "image/png" | "image/webp" | "image/gif") {
                            return Err(MultimodalError::Unsupported);
                        }
                        let data = budget.read(assets, view, message.event_id, &asset)?;
                        let url = format!("data:{};base64,{data}", asset.media_type);
                        match dialect {
                            OpenAiImageDialect::Responses => json!({"type":"input_image", "image_url":url}),
                            OpenAiImageDialect::ChatCompletions => json!({"type":"image_url", "image_url":{"url":url}}),
                        }
                    }
                    TimelineMessagePart::Audio { .. } | TimelineMessagePart::File { .. } => return Err(MultimodalError::Unsupported),
                };
                parts.push(value);
            }
            Ok((
                message.event_id,
                json!({"role":provider_role(message.role, "assistant"), "content":parts}),
            ))
        })
        .collect()
}
