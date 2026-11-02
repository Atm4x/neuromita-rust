use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::GenerationError;
use crate::timeline::domain::CharacterStateDelta;

pub const RESPONSE_PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SegmentIntent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "object_or_json_string")]
    pub payload: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub name: String,
    #[serde(default, deserialize_with = "object_or_json_string")]
    pub args: Map<String, Value>,
}

fn object_or_json_string<'de, D>(deserializer: D) -> Result<Map<String, Value>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Object(fields) => Ok(fields),
        Value::String(raw) => {
            serde_json::from_str::<Map<String, Value>>(&raw).map_err(serde::de::Error::custom)
        }
        _ => Err(serde::de::Error::custom(
            "expected JSON object or encoded object",
        )),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResponseSegment {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emotions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub idle_animations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub movement_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visual_effects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clothes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub music: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub face_params: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<SegmentIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_game: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_game: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_sleep: Option<bool>,
}

impl ResponseSegment {
    pub fn spoken(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn has_unsupported_content(&self) -> bool {
        !self.animations.is_empty()
            || !self.idle_animations.is_empty()
            || !self.commands.is_empty()
            || !self.movement_modes.is_empty()
            || !self.visual_effects.is_empty()
            || !self.clothes.is_empty()
            || !self.music.is_empty()
            || !self.interactions.is_empty()
            || !self.face_params.is_empty()
            || !self.intents.is_empty()
            || self.start_game.is_some()
            || self.end_game.is_some()
            || self.target.is_some()
            || self.hint.is_some()
            || self.allow_sleep.is_some()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WorkingState {
    pub focus: String,
    pub situation: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_loops: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StructuredResponse {
    pub reasoning: Option<String>,
    pub image_description: Option<String>,
    pub segments: Vec<ResponseSegment>,
    pub secret_exposed: Option<bool>,
    pub attitude_change: f64,
    pub boredom_change: f64,
    pub stress_change: f64,
    pub memory_add: Option<Vec<String>>,
    pub memory_update: Option<Vec<String>>,
    pub memory_delete: Option<Vec<String>>,
    pub memory_merge: Option<Vec<String>>,
    pub reminder_add: Option<Vec<String>>,
    pub reminder_delete: Option<Vec<String>>,
    pub timer_add: Option<Vec<String>>,
    pub entities: Option<Vec<String>>,
    pub relations: Option<Vec<String>>,
    pub tool_call: Option<ToolCall>,
    pub custom_fields: Option<Map<String, Value>>,
    pub working_state: Option<WorkingState>,
}

impl StructuredResponse {
    pub fn spoken(text: impl Into<String>) -> Self {
        Self {
            segments: vec![ResponseSegment::spoken(text)],
            ..Self::default()
        }
    }

    pub fn parse(raw: &str) -> Result<Self, GenerationError> {
        let result: Self = serde_json::from_str(raw).map_err(|error| {
            GenerationError::Response(format!("invalid structured response v3: {error}"))
        })?;
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), GenerationError> {
        if self.segments.is_empty() || self.full_text().trim().is_empty() {
            return Err(GenerationError::Response(
                "structured response has no spoken segments".into(),
            ));
        }
        if self.segments.iter().any(|segment| {
            segment
                .intents
                .iter()
                .any(|intent| intent.kind.trim().is_empty())
        }) {
            return Err(GenerationError::Response(
                "structured intent has empty type".into(),
            ));
        }
        for delta in [
            self.attitude_change,
            self.boredom_change,
            self.stress_change,
        ] {
            if !delta.is_finite() {
                return Err(GenerationError::Response(
                    "nonfinite character state delta".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn state_delta(&self) -> CharacterStateDelta {
        CharacterStateDelta {
            attitude: self.attitude_change,
            boredom: self.boredom_change,
            stress: self.stress_change,
        }
    }

    pub fn full_text(&self) -> Arc<str> {
        Arc::from(
            self.segments
                .iter()
                .map(|segment| segment.text.trim())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    pub fn ensure_supported_effects(&self) -> Result<(), GenerationError> {
        if self
            .segments
            .iter()
            .any(ResponseSegment::has_unsupported_content)
            || self.image_description.is_some()
            || self.secret_exposed.is_some()
            || self.memory_add.is_some()
            || self.memory_update.is_some()
            || self.memory_delete.is_some()
            || self.memory_merge.is_some()
            || self.reminder_add.is_some()
            || self.reminder_delete.is_some()
            || self.timer_add.is_some()
            || self.entities.is_some()
            || self.relations.is_some()
            || self.tool_call.is_some()
            || self.custom_fields.is_some()
            || self.working_state.is_some()
        {
            return Err(GenerationError::Response(
                "structured response contains fields without an enabled executor".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn gemini_message_schema() -> Value {
    serde_json::from_str(include_str!("structured_message_gemini.json"))
        .expect("checked-in Gemini schema derived from Python v3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_python_v3_text_and_preserve_order() {
        let parsed =
            StructuredResponse::parse(r#"{"segments":[{"text":"Привет"},{"text":"мир"}]}"#)
                .unwrap();
        assert_eq!(parsed.full_text().as_ref(), "Привет мир");
        assert_eq!(RESPONSE_PROTOCOL_VERSION, 3);
        parsed.ensure_supported_effects().unwrap();
    }

    #[test]
    fn structured_fields_roundtrip_without_executing_actions() {
        let data = json!({
            "segments": [{"text":"hello", "intents":[{"type":"inventory.collect", "payload":"{\"object\":\"apple\"}"}],
                "emotions":["happy"], "target":"KindMita"}],
            "memory_add":["high|Player prefers tea"],
            "tool_call":{"name":"search", "args":"{\"query\":\"tea\"}"},
            "working_state":{"focus":"conversation"}
        });
        let parsed = StructuredResponse::parse(&data.to_string()).unwrap();
        assert_eq!(parsed.segments[0].intents[0].payload["object"], "apple");
        assert_eq!(parsed.tool_call.as_ref().unwrap().args["query"], "tea");
        assert!(parsed.ensure_supported_effects().is_err());
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["segments"][0]["target"],
            "KindMita"
        );
    }

    #[test]
    fn missing_segment_text_is_invalid_even_when_other_segments_are_valid() {
        assert!(StructuredResponse::parse(r#"{"segments":[{}, {"text":"valid"}]}"#).is_err());
    }

    #[test]
    fn rejects_malformed_or_unimplemented_effects() {
        for data in [
            r#"{"segments":[]}"#,
            r#"{"segments":[{"text":" "}]}"#,
            r#"{"segments":[{"text":"hi","unexpected_action":true}]}"#,
            r#"{"segments":[{"text":"hi"}],"tool_call":{"name":"search","args":"not-json"}}"#,
            "plain text",
        ] {
            assert!(StructuredResponse::parse(data).is_err(), "{data}");
        }
        let proposed =
            StructuredResponse::parse(r#"{"segments":[{"text":"hello", "commands":["run"]}]}"#)
                .unwrap();
        assert!(proposed.ensure_supported_effects().is_err());
    }

    #[test]
    fn state_changes_are_supported_but_other_side_effects_are_not() {
        let response = StructuredResponse::parse(
            r#"{"segments":[{"text":"hi"}],"attitude_change":1.25,"stress_change":-2.5}"#,
        )
        .unwrap();
        response.ensure_supported_effects().unwrap();
        assert_eq!(response.state_delta().attitude, 1.25);
        assert_eq!(response.state_delta().stress, -2.5);
        let unsupported = StructuredResponse::parse(
            r#"{"segments":[{"text":"hi"}],"attitude_change":1,"memory_add":["remember"]}"#,
        )
        .unwrap();
        assert!(unsupported.ensure_supported_effects().is_err());
    }

    #[test]
    fn provider_schema_is_capability_limited() {
        let schema = gemini_message_schema();
        assert_eq!(
            schema["properties"]["segments"]["items"]["properties"]["emotions"]["type"],
            "array"
        );
        assert_eq!(schema["required"], json!(["segments"]));
        assert_eq!(
            schema["properties"]["segments"]["items"]["required"],
            json!(["text"])
        );
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .get("tool_call")
                .is_none()
        );
        assert!(
            schema["properties"]["segments"]["items"]["properties"]
                .as_object()
                .unwrap()
                .get("commands")
                .is_none()
        );
    }
}
