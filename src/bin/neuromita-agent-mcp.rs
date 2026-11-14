use std::{net::SocketAddr, time::Duration};

use neuromita::agent_ipc::{call_local_control, run_local_control_scenario};
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

struct NeuroMita {
    core: SocketAddr,
    allow_write: bool,
    core_debug: bool,
    allow_content: bool,
    timeout: Duration,
}

impl ServerHandler for NeuroMita {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("neuromita", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![
            inspection_tool(
                "inspect_core",
                "Read a sanitized Core snapshot.",
                &["scope"],
                json!({
                    "scope": {"type":"string", "enum":["capabilities","lifecycle","timeline","character","requests"]},
                    "character": {"type":"string", "description":"Canonical character ID for scope=character"}
                }),
            ),
            inspection_tool(
                "inspect_presenter",
                "Read a live sanitized Presenter snapshot. Chat requires character.",
                &["surface"],
                json!({
                    "surface": {"type":"string", "enum":["chat","ai_runtime","connections","speech","releases","audio_input","voice","guide"]},
                    "character": {"type":"string", "description":"Canonical character ID for surface=chat"}
                }),
            ),
            inspection_tool(
                "inspect_event_visibility",
                "Check whether a canonical Timeline event is visible to one character, without reading its content.",
                &["character", "event_id"],
                json!({
                    "character": {"type":"string", "description":"Canonical character ID"},
                    "event_id": {"type":"string", "format":"uuid"}
                }),
            ),
            inspection_tool(
                "inspect_gamelink",
                "Read actual GameLink connection/session, delivery ACK and playback state by session/request ID. No session tokens, audio or Unity error text. An ACK is not visual Unity proof.",
                &[],
                json!({
                    "session_id": {"type":"string", "format":"uuid"},
                    "request_id": {"type":"string", "format":"uuid", "description":"Requires session_id"}
                }),
            ),
            inspection_tool(
                "inspect_onboarding",
                "Inspect persisted revision and Guide state from actual Core settings, not GUI.",
                &[],
                json!({}),
            ),
            inspection_tool(
                "inspect_presets",
                "Read active API preset and saved preset names/IDs without credentials.",
                &[],
                json!({}),
            ),
            inspection_tool(
                "inspect_request",
                "Read a single request's revision, state, and terminal outcome from the real Presenter.",
                &["request_id"],
                json!({"request_id": {"type":"string", "format":"uuid"}}),
            ),
            inspection_tool(
                "presenter_conversation",
                "Read a bounded selection-consistent ConversationPresenter snapshot by character. Message IDs, roles, sequence, active preset; --allow-content additionally enables actual text.",
                &["character"], json!({
                    "character":{"type":"string"},
                    "limit":{"type":"integer","minimum":1,"maximum":100},
                    "include_content":{"type":"boolean"}
                }),
            ),
            inspection_tool(
                "presenter_audio_input_devices",
                "Discover real microphone devices using the application's AudioInputPresenter (does not open any device).",
                &[], json!({}),
            ),
            Tool::new("presenter_chat",
                "Act through the exact GUI ChatPresenter: reply, react, initiate, or cancel. Separate from low-level Core debug. Mutations need --allow-write. Use inspect_request to observe generation.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["reply","react","initiate","cancel"]},
                        "character":{"type":"string"},
                        "text":{"type":"string","maxLength":32768},
                        "event_id":{"type":"string","format":"uuid"},
                        "request_id":{"type":"string","format":"uuid"},
                        "instruction":{"type":"string","maxLength":8192}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_audio_input",
                "Start or stop the real GUI microphone loopback test. Use presenter_audio_input_devices to get backend and identity, then inspect_presenter(surface=audio_input) for level/error.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["start_test","stop_test"]},
                        "backend":{"type":"string","maxLength":64},
                        "identity":{"type":"string","maxLength":512}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_guide",
                "Set Guide level, page or completion via the actual OnboardingSettingsPresenter; inspect_presenter(surface=guide) afterwards. Does not claim GUI navigation was rendered.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["field","value"])),
                    ("properties".into(),json!({
                        "field":{"type":"string","enum":["level","page","completed"]},
                        "value":{"oneOf":[{"type":"string","enum":["basic","advanced","full"]},
                            {"type":"integer","minimum":0,"maximum":255},{"type":"boolean"}]}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_voice",
                "Set live voice volume via the GUI VoiceSettingsPresenter; 0..100.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["volume"])),
                    ("properties".into(),json!({"volume":{"type":"integer","minimum":0,"maximum":100}})),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_refresh",
                "Trigger refresh through ReleasesPresenter or RuntimeStatePresenter; read their snapshots separately.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["surface"])),
                    ("properties".into(),json!({"surface":{"type":"string","enum":["releases","ai_runtime"]}})),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("core_debug_command",
                "Privileged raw typed Core LocalControl command. Requires --core-debug, and --allow-write for mutation. Strict allowlist, no unrestricted reflection. Use Presenter tools for ordinary UI acceptance.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["method"])),
                    ("properties".into(),json!({
                        "method":{"type":"string", "enum":["ping","methods","server_status","debug_inspect","game_link_inspect","presenter_inspect","settings","publish_message","reply","react","initiate","cancel","diagnostics","diagnostics_recent","diagnostics_errors","set_volume","set_active_preset"]},
                        "params":{"type":"object","maxProperties":20}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new(
                "select_active_preset",
                "Change the active API preset; requires MCP --allow-write. Read inspect_presets first.",
                serde_json::Map::from_iter([
                    ("type".into(), json!("object")),
                    ("additionalProperties".into(), json!(false)),
                    ("required".into(), json!(["preset_id"])),
                    ("properties".into(), json!({
                        "preset_id":{"type":"integer","minimum":0,"maximum":4294967295_u64},
                        "expected_revision":{"type":"integer","minimum":0}
                    })),
                ]),
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new(
                "run_scenario",
                "Run 1..32 Core JSONL steps on ONE owner TCP connection. Supports wait_terminal, expect/expect_terminal, $last_request_id and $last_event_id; writes require MCP --allow-write. Sanitized results only.",
                serde_json::Map::from_iter([
                    ("type".into(), json!("object")),
                    ("additionalProperties".into(), json!(false)),
                    ("required".into(), json!(["steps"])),
                    ("properties".into(), json!({
                        "steps":{"type":"array","minItems":1,"maxItems":32,
                            "items":{"type":"object","required":["command"],
                                "properties":{"command":{"type":"string"}},"additionalProperties":true}}
                    })),
                ]),
            ).with_annotations(ToolAnnotations::new().read_only(false)),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        if request.name == "presenter_conversation"
            && args.get("include_content") == Some(&Value::Bool(true))
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "include_content requires --allow-content",
                None,
            ));
        }
        if request.name == "core_debug_command" {
            if !self.core_debug {
                return Err(ErrorData::invalid_params(
                    "MCP was started without --core-debug",
                    None,
                ));
            }
            let method = args
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| ErrorData::invalid_params("missing method", None))?;
            let mut command = args.get("params").cloned().unwrap_or_else(|| json!({}));
            let object = command
                .as_object_mut()
                .ok_or_else(|| ErrorData::invalid_params("params must be an object", None))?;
            if args.keys().any(|key| key != "method" && key != "params")
                || object.contains_key("command")
            {
                return Err(ErrorData::invalid_params(
                    "unexpected debug command fields",
                    None,
                ));
            }
            let allowed = matches!(
                method,
                "ping"
                    | "methods"
                    | "server_status"
                    | "debug_inspect"
                    | "game_link_inspect"
                    | "presenter_inspect"
                    | "settings"
                    | "publish_message"
                    | "reply"
                    | "react"
                    | "initiate"
                    | "cancel"
                    | "diagnostics"
                    | "diagnostics_recent"
                    | "diagnostics_errors"
                    | "set_volume"
                    | "set_active_preset"
            );
            if !allowed {
                return Err(ErrorData::invalid_params("unsupported Core method", None));
            }
            let readonly = matches!(
                method,
                "ping"
                    | "methods"
                    | "server_status"
                    | "debug_inspect"
                    | "game_link_inspect"
                    | "presenter_inspect"
                    | "settings"
                    | "diagnostics"
                    | "diagnostics_recent"
                    | "diagnostics_errors"
            );
            if !readonly && !self.allow_write {
                return Err(ErrorData::invalid_params(
                    "Core write requires --allow-write",
                    None,
                ));
            }
            object.insert("command".into(), json!(method));
            if serde_json::to_vec(&command).map_or(true, |bytes| bytes.len() > 64 * 1024) {
                return Err(ErrorData::invalid_params(
                    "Core command exceeds 64 KiB",
                    None,
                ));
            }
            let result = match call_local_control(self.core, self.timeout, command).await {
                Ok(response) if response["type"] != "error" => CallToolResult::structured(response),
                Ok(response) => CallToolResult::structured_error(json!({"code":response["code"]})),
                Err(error) => CallToolResult::structured_error(json!({"error":error.to_string()})),
            };
            return Ok(result.into());
        }
        if request.name == "run_scenario" {
            if args.len() != 1 || !args.contains_key("steps") {
                return Err(ErrorData::invalid_params("expected only steps", None));
            }
            let steps = args
                .get("steps")
                .and_then(Value::as_array)
                .ok_or_else(|| ErrorData::invalid_params("steps must be an array", None))?
                .clone();
            if !self.allow_content
                && steps.iter().any(|step| {
                    step.get("command").and_then(Value::as_str) == Some("presenter_conversation")
                        && step.get("include_content") == Some(&Value::Bool(true))
                })
            {
                return Err(ErrorData::invalid_params(
                    "scenario content requires --allow-content",
                    None,
                ));
            }
            if !self.core_debug
                && steps.iter().any(|step| {
                    !matches!(
                        step.get("command").and_then(Value::as_str),
                        Some("presenter_action" | "presenter_conversation" | "presenter_inspect")
                    )
                })
            {
                return Err(ErrorData::invalid_params(
                    "raw Core scenarios require --core-debug",
                    None,
                ));
            }
            let response =
                match run_local_control_scenario(self.core, self.timeout, self.allow_write, steps)
                    .await
                {
                    Ok(value) if value["ok"] == true => CallToolResult::structured(value),
                    Ok(value) => CallToolResult::structured_error(value),
                    Err(error) => {
                        CallToolResult::structured_error(json!({"error":error.to_string()}))
                    }
                };
            return Ok(response.into());
        }
        if matches!(
            request.name.as_ref(),
            "inspect_core" | "inspect_event_visibility" | "inspect_request" | "inspect_gamelink"
        ) && !self.core_debug
        {
            return Err(ErrorData::invalid_params(
                "direct Core inspection requires --core-debug",
                None,
            ));
        }
        if matches!(
            request.name.as_ref(),
            "presenter_chat"
                | "presenter_audio_input"
                | "presenter_guide"
                | "presenter_voice"
                | "presenter_refresh"
        ) && !self.allow_write
        {
            return Err(ErrorData::invalid_params(
                "Presenter actions require --allow-write",
                None,
            ));
        }
        if request.name == "select_active_preset" && !self.allow_write {
            return Err(ErrorData::invalid_params(
                "MCP was started without --allow-write",
                None,
            ));
        }
        let command = command_for_tool(request.name.as_ref(), args)?;
        let response = match call_local_control(self.core, self.timeout, command).await {
            Ok(response) if response["type"] != "error" => CallToolResult::structured(response),
            Ok(response) => CallToolResult::structured_error(json!({"code": response["code"]})),
            Err(error) => CallToolResult::structured_error(json!({"error": error.to_string()})),
        };
        Ok(response.into())
    }
}

fn command_for_tool(name: &str, mut args: Map<String, Value>) -> Result<Value, ErrorData> {
    let string = |args: &mut Map<String, Value>, key| -> Result<String, ErrorData> {
        args.remove(key)
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| ErrorData::invalid_params(format!("missing or invalid {key}"), None))
    };
    let command = match name {
        "inspect_core" => {
            let scope = string(&mut args, "scope")?;
            if !matches!(
                scope.as_str(),
                "capabilities" | "lifecycle" | "timeline" | "character" | "requests"
            ) {
                return Err(ErrorData::invalid_params("unknown core scope", None));
            }
            let mut command = json!({"command":"debug_inspect", "scope":scope});
            if let Some(character) = args.remove("character") {
                if scope != "character" || !character.is_string() {
                    return Err(ErrorData::invalid_params(
                        "character not allowed for this scope",
                        None,
                    ));
                }
                command["character"] = character;
            }
            command
        }
        "inspect_presenter" => {
            let surface = string(&mut args, "surface")?;
            if !matches!(
                surface.as_str(),
                "chat"
                    | "ai_runtime"
                    | "connections"
                    | "speech"
                    | "releases"
                    | "audio_input"
                    | "voice"
                    | "guide"
            ) {
                return Err(ErrorData::invalid_params("unknown presenter surface", None));
            }
            let character = args.remove("character");
            if (surface == "chat") != character.is_some()
                || character.as_ref().is_some_and(|v| !v.is_string())
            {
                return Err(ErrorData::invalid_params(
                    "chat requires character; other surfaces forbid it",
                    None,
                ));
            }
            let mut command = json!({"command":"presenter_inspect", "surface":surface});
            if let Some(character) = character {
                command["character"] = character;
            }
            command
        }
        "inspect_event_visibility" => {
            let character = string(&mut args, "character")?;
            let event_id = uuid_field(&mut args, "event_id")?;
            json!({"command":"debug_inspect", "scope":"character", "character":character, "event_id":event_id})
        }
        "inspect_request" => {
            let request_id = uuid_field(&mut args, "request_id")?;
            json!({"command":"debug_inspect", "scope":"request", "request_id":request_id})
        }
        "inspect_gamelink" => {
            let session_id = args.remove("session_id");
            let request_id = args.remove("request_id");
            if request_id.is_some() && session_id.is_none() {
                return Err(ErrorData::invalid_params(
                    "request_id requires session_id",
                    None,
                ));
            }
            let mut command = json!({"command":"game_link_inspect"});
            for (name, value) in [("session_id", session_id), ("request_id", request_id)] {
                if let Some(value) = value {
                    let uuid = value
                        .as_str()
                        .and_then(|v| Uuid::parse_str(v).ok())
                        .ok_or_else(|| {
                            ErrorData::invalid_params(format!("invalid {name} UUID"), None)
                        })?;
                    command[name] = json!(uuid);
                }
            }
            command
        }
        "presenter_conversation" => {
            let character = string(&mut args, "character")?;
            let limit = args
                .remove("limit")
                .map(|v| v.as_u64())
                .unwrap_or(Some(40))
                .filter(|v| (1..=100).contains(v))
                .ok_or_else(|| ErrorData::invalid_params("limit must be 1..100", None))?;
            let include_content = args
                .remove("include_content")
                .map(|v| v.as_bool())
                .unwrap_or(Some(false))
                .ok_or_else(|| {
                    ErrorData::invalid_params("include_content must be boolean", None)
                })?;
            json!({"command":"presenter_conversation","character":character,
                "limit":limit,"include_content":include_content})
        }
        "presenter_audio_input_devices" => {
            json!({"command":"presenter_action","action":{"action":"audio_devices"}})
        }
        "presenter_chat" => {
            let action = string(&mut args, "action")?;
            let mut payload = json!({"action":format!("chat_{action}")});
            match action.as_str() {
                "reply" => {
                    payload["character"] = json!(string(&mut args, "character")?);
                    payload["text"] = json!(string(&mut args, "text")?);
                }
                "react" => {
                    payload["character"] = json!(string(&mut args, "character")?);
                    payload["event_id"] = json!(uuid_field(&mut args, "event_id")?);
                    payload["instruction"] = json!(args.remove("instruction").unwrap_or(json!("")));
                }
                "initiate" => {
                    payload["character"] = json!(string(&mut args, "character")?);
                    payload["instruction"] = json!(args.remove("instruction").unwrap_or(json!("")));
                }
                "cancel" => payload["request_id"] = json!(uuid_field(&mut args, "request_id")?),
                _ => {
                    return Err(ErrorData::invalid_params(
                        "unknown presenter_chat action",
                        None,
                    ));
                }
            }
            json!({"command":"presenter_action","action":payload})
        }
        "presenter_audio_input" => {
            let action = string(&mut args, "action")?;
            let payload = match action.as_str() {
                "start_test" => {
                    json!({"action":"audio_start_test","backend":string(&mut args,"backend")?,"identity":string(&mut args,"identity")?})
                }
                "stop_test" => json!({"action":"audio_stop_test"}),
                _ => return Err(ErrorData::invalid_params("unknown audio action", None)),
            };
            json!({"command":"presenter_action","action":payload})
        }
        "presenter_guide" => {
            let field = string(&mut args, "field")?;
            let value = args
                .remove("value")
                .ok_or_else(|| ErrorData::invalid_params("missing value", None))?;
            let (action, key) = match field.as_str() {
                "level" if matches!(value.as_str(), Some("basic" | "advanced" | "full")) => {
                    ("guide_set_level", "level")
                }
                "page" if value.as_u64().is_some_and(|n| n <= 255) => ("guide_set_page", "page"),
                "completed" if value.is_boolean() => ("guide_set_completed", "completed"),
                _ => {
                    return Err(ErrorData::invalid_params(
                        "invalid Guide field or value",
                        None,
                    ));
                }
            };
            json!({"command":"presenter_action","action":{"action":action,key:value}})
        }
        "presenter_voice" => {
            let volume = args
                .remove("volume")
                .and_then(|v| v.as_u64())
                .filter(|n| *n <= 100)
                .ok_or_else(|| ErrorData::invalid_params("volume must be 0..100", None))?;
            json!({"command":"presenter_action","action":{"action":"voice_set_volume","volume":volume}})
        }
        "presenter_refresh" => {
            let surface = string(&mut args, "surface")?;
            let action = match surface.as_str() {
                "releases" => "releases_refresh",
                "ai_runtime" => "runtime_refresh",
                _ => return Err(ErrorData::invalid_params("unknown refresh surface", None)),
            };
            json!({"command":"presenter_action","action":{"action":action}})
        }
        "inspect_presets" => json!({"command":"debug_inspect", "scope":"presets"}),
        "inspect_onboarding" => json!({"command":"debug_inspect", "scope":"onboarding"}),
        "select_active_preset" => {
            let id = args
                .remove("preset_id")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| ErrorData::invalid_params("preset_id must be a u32", None))?;
            let mut command = json!({"command":"set_active_preset", "preset_id":id});
            if let Some(revision) = args.remove("expected_revision") {
                if !revision.is_u64() {
                    return Err(ErrorData::invalid_params(
                        "expected_revision must be a u64",
                        None,
                    ));
                }
                command["expected_revision"] = revision;
            }
            command
        }
        _ => return Err(ErrorData::invalid_params("unknown NeuroMita tool", None)),
    };
    if !args.is_empty() {
        return Err(ErrorData::invalid_params("unknown tool arguments", None));
    }
    Ok(command)
}

fn uuid_field(args: &mut Map<String, Value>, key: &str) -> Result<Uuid, ErrorData> {
    let value = args
        .remove(key)
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| ErrorData::invalid_params(format!("missing or invalid {key}"), None))?;
    Uuid::parse_str(&value)
        .map_err(|_| ErrorData::invalid_params(format!("invalid {key} UUID"), None))
}

fn inspection_tool(
    name: &'static str,
    description: &'static str,
    required: &[&str],
    properties: Value,
) -> Tool {
    let schema = serde_json::Map::from_iter([
        ("type".into(), json!("object")),
        ("additionalProperties".into(), json!(false)),
        ("required".into(), json!(required)),
        ("properties".into(), properties),
    ]);
    Tool::new(name, description, schema).with_annotations(ToolAnnotations::new().read_only(true))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut core = None;
    let mut allow_write = false;
    let mut core_debug = false;
    let mut allow_content = false;
    let mut timeout = Duration::from_secs(60);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--core" => core = Some(args.next().ok_or("missing --core value")?.parse()?),
            "--allow-write" => allow_write = true,
            "--core-debug" => core_debug = true,
            "--allow-content" => allow_content = true,
            "--timeout-ms" => {
                let value: u64 = args.next().ok_or("missing --timeout-ms value")?.parse()?;
                if !(1..=300_000).contains(&value) {
                    return Err("invalid timeout".into());
                }
                timeout = Duration::from_millis(value);
            }
            _ => return Err(
                "usage: neuromita-agent-mcp --core LOOPBACK:PORT [--allow-write] [--core-debug] [--allow-content] [--timeout-ms N]"
                    .into(),
            ),
        }
    }
    let core: SocketAddr = core.ok_or("--core LOOPBACK:PORT is required")?;
    if !core.ip().is_loopback() {
        return Err("LocalControl address must be loopback".into());
    }
    NeuroMita {
        core,
        allow_write,
        core_debug,
        allow_content,
        timeout,
    }
    .serve(stdio())
    .await?
    .waiting()
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn typed_read_only_tools_map_to_core_contract() {
        let event_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let event = command_for_tool(
            "inspect_event_visibility",
            args(json!({"character":"Crazy", "event_id":event_id})),
        )
        .unwrap();
        assert_eq!(event["command"], "debug_inspect");
        assert_eq!(event["scope"], "character");
        assert_eq!(event["event_id"], json!(event_id));
        let request =
            command_for_tool("inspect_request", args(json!({"request_id":request_id}))).unwrap();
        assert_eq!(request["scope"], "request");
        let presets = command_for_tool("inspect_presets", args(json!({}))).unwrap();
        assert_eq!(presets["scope"], "presets");
        let selected = command_for_tool(
            "select_active_preset",
            args(json!({"preset_id":12,"expected_revision":4})),
        )
        .unwrap();
        assert_eq!(selected["command"], "set_active_preset");
        assert_eq!(selected["preset_id"], 12);
        assert!(command_for_tool("select_active_preset", args(json!({"preset_id":-1}))).is_err());
        let game = command_for_tool(
            "inspect_gamelink",
            args(json!({"session_id":Uuid::new_v4(), "request_id":Uuid::new_v4()})),
        )
        .unwrap();
        assert_eq!(game["command"], "game_link_inspect");
        assert!(
            command_for_tool(
                "inspect_gamelink",
                args(json!({"request_id":Uuid::new_v4()}))
            )
            .is_err()
        );
        assert!(
            command_for_tool("inspect_gamelink", args(json!({"session_id":"not-uuid"}))).is_err()
        );
        assert!(
            command_for_tool(
                "inspect_gamelink",
                args(json!({"session_id":Uuid::new_v4(),"token":"secret"}))
            )
            .is_err()
        );
        let chat = command_for_tool(
            "presenter_chat",
            args(json!({"action":"reply","character":"Crazy","text":"test"})),
        )
        .unwrap();
        assert_eq!(chat["command"], "presenter_action");
        assert_eq!(chat["action"]["action"], "chat_reply");
        assert!(
            command_for_tool(
                "presenter_chat",
                args(json!({"action":"cancel","request_id":"bad"}))
            )
            .is_err()
        );
        let guide = command_for_tool(
            "presenter_guide",
            args(json!({"field":"level","value":"advanced"})),
        )
        .unwrap();
        assert_eq!(guide["action"]["action"], "guide_set_level");
        assert!(
            command_for_tool("presenter_guide", args(json!({"field":"page","value":999}))).is_err()
        );
        let conversation = command_for_tool(
            "presenter_conversation",
            args(json!({"character":"Crazy","limit":20})),
        )
        .unwrap();
        assert_eq!(conversation["command"], "presenter_conversation");
        assert_eq!(conversation["include_content"], false);
        let speech =
            command_for_tool("inspect_presenter", args(json!({"surface":"speech"}))).unwrap();
        assert_eq!(speech["surface"], "speech");
        assert!(
            command_for_tool(
                "inspect_presenter",
                args(json!({"surface":"chat","character":"Crazy"}))
            )
            .is_ok()
        );
        assert!(
            command_for_tool(
                "inspect_presenter",
                args(json!({"surface":"releases","character":"Crazy"}))
            )
            .is_err()
        );
        assert!(
            command_for_tool(
                "inspect_event_visibility",
                args(json!({"character":"Crazy","event_id":"bad"}))
            )
            .is_err()
        );
        assert!(
            command_for_tool(
                "inspect_request",
                args(json!({"request_id":request_id,"dump":true}))
            )
            .is_err()
        );
        assert!(
            command_for_tool("inspect_core", args(json!({"scope":"unsafe_history_dump"}))).is_err()
        );
    }
}
