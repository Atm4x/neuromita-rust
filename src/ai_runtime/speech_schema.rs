//! Typed, read-only presentation of Python SDK settings_schema contracts.

use serde_json::{Map, Number, Value};

use super::AiRuntimeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechSettingKind {
    String,
    Boolean,
    Integer,
    Number,
}

impl SpeechSettingKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpeechSettingField {
    pub key: String,
    pub kind: SpeechSettingKind,
    pub default: Option<Value>,
    pub choices: Vec<Value>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

impl SpeechSettingField {
    pub fn display_value(&self, overrides: &Map<String, Value>) -> String {
        let value = overrides.get(&self.key).or(self.default.as_ref());
        value.map_or_else(String::new, scalar_text)
    }

    pub fn parse_input(&self, input: &str) -> Result<Value, AiRuntimeError> {
        let value = match self.kind {
            SpeechSettingKind::String => Value::String(input.to_owned()),
            SpeechSettingKind::Boolean => match input {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(invalid(&self.key, "boolean value must be true or false")),
            },
            SpeechSettingKind::Integer => {
                let value = input
                    .parse::<i64>()
                    .map_err(|_| invalid(&self.key, "invalid integer"))?;
                Value::Number(Number::from(value))
            }
            SpeechSettingKind::Number => {
                let value = input
                    .parse::<f64>()
                    .map_err(|_| invalid(&self.key, "invalid number"))?;
                Value::Number(
                    Number::from_f64(value)
                        .ok_or_else(|| invalid(&self.key, "non-finite number"))?,
                )
            }
        };
        self.check(&value)?;
        Ok(value)
    }

    pub fn check(&self, value: &Value) -> Result<(), AiRuntimeError> {
        let valid_type = match self.kind {
            SpeechSettingKind::String => value.is_string(),
            SpeechSettingKind::Boolean => value.is_boolean(),
            SpeechSettingKind::Integer => value.is_i64() || value.is_u64(),
            SpeechSettingKind::Number => value.is_number(),
        };
        if !valid_type {
            return Err(invalid(&self.key, "wrong value type"));
        }
        if !self.choices.is_empty() && !self.choices.contains(value) {
            return Err(invalid(&self.key, "value not in Python plugin choices"));
        }
        if let Some(number) = value.as_f64() {
            if !number.is_finite()
                || self.minimum.is_some_and(|bound| number < bound)
                || self.maximum.is_some_and(|bound| number > bound)
            {
                return Err(invalid(&self.key, "value outside Python plugin bounds"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SpeechSettingsSchema {
    pub fields: Vec<SpeechSettingField>,
}

impl SpeechSettingsSchema {
    pub fn parse(schema: &Value) -> Result<Self, AiRuntimeError> {
        let items = schema
            .as_object()
            .ok_or_else(|| invalid("schema", "expected object"))?;
        if items.len() > 128 || serde_json::to_vec(schema).is_ok_and(|bytes| bytes.len() > 131_072)
        {
            return Err(invalid("schema", "too many or too large fields"));
        }
        let mut fields = Vec::with_capacity(items.len());
        for (key, value) in items {
            if key.is_empty() || key.len() > 128 || !key.is_ascii() {
                return Err(invalid(key, "invalid field name"));
            }
            let definition = value
                .as_object()
                .ok_or_else(|| invalid(key, "expected definition object"))?;
            let kind = match definition.get("type").and_then(Value::as_str) {
                Some("string") => SpeechSettingKind::String,
                Some("boolean") => SpeechSettingKind::Boolean,
                Some("integer") => SpeechSettingKind::Integer,
                Some("number") => SpeechSettingKind::Number,
                _ => return Err(invalid(key, "unsupported Python settings type")),
            };
            let choices = match definition.get("choices") {
                None => Vec::new(),
                Some(Value::Array(values)) if values.len() <= 256 => values.clone(),
                _ => return Err(invalid(key, "invalid choice list")),
            };
            let minimum = definition
                .get("min")
                .map(|v| v.as_f64().ok_or_else(|| invalid(key, "invalid minimum")))
                .transpose()?;
            let maximum = definition
                .get("max")
                .map(|v| v.as_f64().ok_or_else(|| invalid(key, "invalid maximum")))
                .transpose()?;
            if minimum.is_some_and(|v| !v.is_finite())
                || maximum.is_some_and(|v| !v.is_finite())
                || minimum.zip(maximum).is_some_and(|(min, max)| min > max)
            {
                return Err(invalid(key, "invalid numerical bounds"));
            }
            let field = SpeechSettingField {
                key: key.clone(),
                kind,
                choices,
                minimum,
                maximum,
                default: definition.get("default").cloned(),
            };
            if let Some(default) = &field.default {
                field.check(default)?;
            }
            for choice in &field.choices {
                field.check_type_only(choice)?;
            }
            fields.push(field);
        }
        Ok(Self { fields })
    }

    pub fn field(&self, key: &str) -> Option<&SpeechSettingField> {
        self.fields.iter().find(|item| item.key == key)
    }

    pub fn validate_overrides(&self, overrides: &Map<String, Value>) -> Result<(), AiRuntimeError> {
        for (key, value) in overrides {
            let field = self
                .field(key)
                .ok_or_else(|| invalid(key, "unknown plugin field"))?;
            field.check(value)?;
        }
        Ok(())
    }
}

impl SpeechSettingField {
    fn check_type_only(&self, value: &Value) -> Result<(), AiRuntimeError> {
        let type_ok = match self.kind {
            SpeechSettingKind::String => value.is_string(),
            SpeechSettingKind::Boolean => value.is_boolean(),
            SpeechSettingKind::Integer => value.is_i64() || value.is_u64(),
            SpeechSettingKind::Number => value.is_number(),
        };
        if !type_ok {
            Err(invalid(&self.key, "choice type mismatch"))
        } else {
            Ok(())
        }
    }
}

pub fn scalar_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn invalid(field: &str, reason: &str) -> AiRuntimeError {
    AiRuntimeError::Protocol(format!("settings_schema {field}: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_scalar_schema_preserves_types_choices_and_bounds() {
        let schema = SpeechSettingsSchema::parse(&json!({
            "enabled": {"type":"boolean", "default":false},
            "device": {"type":"string", "default":"cpu", "choices":["cpu","cuda:0"]},
            "sample_rate": {"type":"integer", "default":48000, "choices":[8000,24000,48000]},
            "volume": {"type":"number", "default":1.0, "min":0.0, "max":2.0}
        }))
        .unwrap();
        assert_eq!(
            schema
                .field("device")
                .unwrap()
                .parse_input("cuda:0")
                .unwrap(),
            json!("cuda:0")
        );
        assert!(schema.field("device").unwrap().parse_input("dml").is_err());
        assert_eq!(
            schema
                .field("enabled")
                .unwrap()
                .parse_input("true")
                .unwrap(),
            json!(true)
        );
        assert_eq!(
            schema
                .field("sample_rate")
                .unwrap()
                .parse_input("24000")
                .unwrap(),
            json!(24000)
        );
        assert!(
            schema
                .field("sample_rate")
                .unwrap()
                .parse_input("2.5")
                .is_err()
        );
        assert!(schema.field("volume").unwrap().parse_input("NaN").is_err());
        assert!(schema.field("volume").unwrap().parse_input("2.5").is_err());
        assert!(
            schema
                .field("volume")
                .unwrap()
                .parse_input("-0.01")
                .is_err()
        );
        assert!(
            schema
                .validate_overrides(&serde_json::from_value(json!({"sample_rate":8000})).unwrap())
                .is_ok()
        );
        assert!(
            schema
                .validate_overrides(&serde_json::from_value(json!({"unrecognized":1})).unwrap())
                .is_err()
        );
    }

    #[tokio::test]
    async fn reads_python_install_contract_without_installing_or_initializing() {
        use crate::ai_runtime::{AiService, PythonHostConfig, fetch_speech_settings_schema};
        use std::{path::PathBuf, time::Duration};
        let root = std::env::temp_dir().join(format!("speech-schema-{}", uuid::Uuid::new_v4()));
        let plugin = root.join("plugins/demo_tts");
        tokio::fs::create_dir_all(&plugin).await.unwrap();
        tokio::fs::write(plugin.join("plugin.json"),
            r#"{"id":"demo_tts","name":"Demo TTS","kind":"tts","api_version":1,"entrypoint":"plugin.py"}"#).await.unwrap();
        tokio::fs::write(plugin.join("installplan.py"),
            r#"from neuromita_ai_sdk import InstallPlan
class InstallContract:
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {'installed': False}
    def settings_schema(self): return {'voice': {'type': 'string', 'default': 'alpha', 'choices': ['alpha','beta']}, 'volume': {'type': 'number', 'default': 1.0, 'min': 0.0, 'max': 2.0}}
"#).await.unwrap();
        let host = PythonHostConfig {
            python_executable: std::env::var_os("NEUROMITA_TEST_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" })),
            sdk_root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python"),
            plugins_directory: root.join("plugins"),
            models_directory: None,
            request_timeout: Duration::from_secs(8),
        };
        let fields = fetch_speech_settings_schema(&host, AiService::Tts, "demo_tts")
            .await
            .unwrap();
        assert_eq!(
            fields
                .field("voice")
                .unwrap()
                .display_value(&serde_json::Map::new()),
            "alpha"
        );
        assert!(fields.field("volume").unwrap().parse_input("3.0").is_err());
        assert!(
            fetch_speech_settings_schema(&host, AiService::Asr, "demo_tts")
                .await
                .is_err()
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn untrusted_plugin_schema_must_be_scalar_and_consistent() {
        assert!(SpeechSettingsSchema::parse(&json!({"x":{"type":"array"}})).is_err());
        assert!(
            SpeechSettingsSchema::parse(&json!({"x":{"type":"string","default":true}})).is_err()
        );
        assert!(
            SpeechSettingsSchema::parse(&json!({"x":{"type":"integer","default":3,"min":5}}))
                .is_err()
        );
        assert!(
            SpeechSettingsSchema::parse(&json!({"x":{"type":"string","choices":[1]}})).is_err()
        );
    }
}
