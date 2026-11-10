//! User-selected local file ingress into canonical Timeline parts.
//! The caller supplies the asset root; no source path is persisted in Timeline.

use std::{fs::File, io, path::Path};

use serde_json::to_value;
use thiserror::Error;

use crate::identity::CharacterId;

use super::{
    conversation::{ConversationAudience, ConversationMessage, ConversationSpeaker},
    domain::TimelineMessagePart,
    storage::assets::{TimelineAssetError, TimelineAssetStore},
};

#[derive(Debug, Error)]
pub enum LocalAttachmentError {
    #[error("attachment file cannot be opened: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Asset(#[from] TimelineAssetError),
    #[error("attachment message text exceeds 64 KiB")]
    TextTooLarge,
}

fn file_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Imports one file, bounded by the injected store limit. File names and absolute
/// source paths are deliberately not recorded in the canonical content.
pub fn import_local_file(
    store: &TimelineAssetStore,
    path: &Path,
) -> Result<TimelineMessagePart, LocalAttachmentError> {
    let media_type = file_media_type(path);
    let reference = store.import(media_type, File::open(path)?)?;
    let part = if media_type.starts_with("image/") {
        TimelineMessagePart::Image { asset: reference }
    } else if media_type.starts_with("audio/") {
        TimelineMessagePart::Audio { asset: reference }
    } else {
        TimelineMessagePart::File { asset: reference }
    };
    Ok(part)
}

/// Creates one addressed player event, not a second text-only message.
/// No inference request is started: the live provider integration must first
/// support typed parts without violating the MitaView boundary.
pub fn compose_local_file_message(
    character: CharacterId,
    text: String,
    part: TimelineMessagePart,
) -> Result<ConversationMessage, LocalAttachmentError> {
    if text.len() > 64 * 1024 {
        return Err(LocalAttachmentError::TextTooLarge);
    }
    let mut parts = Vec::with_capacity(2);
    if !text.trim().is_empty() {
        parts.push(TimelineMessagePart::Text { text });
    }
    parts.push(part);
    let mut message = ConversationMessage::new(
        "local-attachment",
        ConversationSpeaker::Player,
        to_value(parts).expect("typed message parts serialize to JSON"),
        [character],
    );
    message.message_id = format!("local-attachment:{}", message.id.0);
    message.audience = ConversationAudience::player_to(character);
    Ok(message)
}
