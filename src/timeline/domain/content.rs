use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Content identity is independent of event identity. Timeline owns the
/// reference, while the injected AssetStore owns the corresponding bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineAssetRef {
    pub digest: String,
    pub size: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TimelineMessagePart {
    Text { text: String },
    Image { asset: TimelineAssetRef },
    Audio { asset: TimelineAssetRef },
    File { asset: TimelineAssetRef },
}

impl TimelineMessagePart {
    pub fn asset(&self) -> Option<&TimelineAssetRef> {
        match self {
            Self::Text { .. } => None,
            Self::Image { asset } | Self::Audio { asset } | Self::File { asset } => Some(asset),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimelineContentError {
    #[error("message content is not valid typed parts")]
    Invalid,
    #[error("message content exceeds the part count limit")]
    TooManyParts,
    #[error("media part kind and declared MIME type disagree")]
    WrongMediaType,
}

/// Accept historical string and text-only part objects without silently
/// interpreting unknown media shapes. Unknown fields/types are errors.
pub fn parse_message_content(
    value: &Value,
) -> Result<Vec<TimelineMessagePart>, TimelineContentError> {
    match value {
        Value::String(text) => Ok(vec![TimelineMessagePart::Text { text: text.clone() }]),
        Value::Array(parts) => {
            if parts.len() > 32 {
                return Err(TimelineContentError::TooManyParts);
            }
            parts
                .iter()
                .map(|part| {
                    let Some(object) = part.as_object() else {
                        return Err(TimelineContentError::Invalid);
                    };
                    let parsed = if object.len() == 1 && object.contains_key("text") {
                        TimelineMessagePart::Text {
                            text: object["text"]
                                .as_str()
                                .ok_or(TimelineContentError::Invalid)?
                                .to_owned(),
                        }
                    } else {
                        serde_json::from_value::<TimelineMessagePart>(part.clone())
                            .map_err(|_| TimelineContentError::Invalid)?
                    };
                    match &parsed {
                        TimelineMessagePart::Image { asset }
                            if !asset.media_type.starts_with("image/") =>
                        {
                            Err(TimelineContentError::WrongMediaType)
                        }
                        TimelineMessagePart::Audio { asset }
                            if !asset.media_type.starts_with("audio/") =>
                        {
                            Err(TimelineContentError::WrongMediaType)
                        }
                        _ => Ok(parsed),
                    }
                })
                .collect()
        }
        _ => Err(TimelineContentError::Invalid),
    }
}
