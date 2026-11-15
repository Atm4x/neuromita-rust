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
                    "surface": {"type":"string", "enum":["chat","ai_runtime","connections","speech","releases","audio_input","voice","guide","presets","ai_hub_install","telegram_auth","remote_voice"]},
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
            Tool::new("presenter_presets",
                "Manage the real PresetsPresenter catalog: select, create, move, delete, or update_connection with mandatory revision fence. API key requires --allow-content. Requires --allow-write.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["select","create_unconfigured","move","delete","update_connection"]},
                        "preset_id":{"type":"integer","minimum":0,"maximum":4294967295_u64},
                        "step":{"type":"integer","enum":[-1,1]},
                        "expected_revision":{"type":"integer","minimum":0},
                        "name":{"type":"string","minLength":1,"maxLength":256},
                        "template_id":{"type":"string","maxLength":64},
                        "endpoint":{"type":"string","maxLength":2048},
                        "model":{"type":"string","maxLength":256},
                        "temperature":{"type":"number","minimum":0,"maximum":2},
                        "max_output_tokens":{"type":"integer","minimum":1,"maximum":1000000},
                        "timeout_ms":{"type":"integer","minimum":1,"maximum":600000},
                        "stream":{"type":"boolean"},
                        "max_retries":{"type":"integer","minimum":0,"maximum":4},
                        "send_temperature":{"type":"boolean"},
                        "send_max_output_tokens":{"type":"boolean"},
                        "api_key":{"type":"string","minLength":1,"maxLength":16384}
                    })),
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
            inspection_tool(
                "presenter_speech_inventory",
                "Rescan the application's authoritative AIData speech catalog and attached microphone devices. Same source as GUI RefreshSpeechInventory; read-only, no worker activation.",
                &[],json!({}),
            ),
            Tool::new("presenter_telegram",
                "Manage the SAME GUI Telegram MTProto session: connect, code, password, disconnect or refresh voice. Requires --allow-write. Secret-bearing actions also require --allow-content and NEUROMITA_ENABLE_LOCAL_TELEGRAM_AUTH=1 in the Core host (trusted local machine only; loopback TCP is unauthenticated). No credentials returned. Inspect via inspect_presenter(surface=telegram_auth).",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["connect","code","password","disconnect","refresh_voice"]},
                        "api_id":{"type":"string","maxLength":32},
                        "api_hash":{"type":"string","maxLength":128},
                        "phone":{"type":"string","maxLength":64},
                        "code":{"type":"string","maxLength":32},
                        "password":{"type":"string","maxLength":1024}
                    }))
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_remote_voice",
                "Manage the SAME GUI Fish Audio catalog and preview: add, delete, select, save, preview, cancel. Inspect via inspect_presenter(surface=remote_voice) and wait(target=remote_voice). Saves require expected_revision. No API keys returned; secret updates require --allow-content and Core NEUROMITA_ENABLE_LOCAL_REMOTE_VOICE_SECRETS=1. Requires --allow-write; preview also requires --allow-content.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["add","delete","select","select_with_save","save","preview","cancel"]},
                        "expected_revision":{"type":"integer","minimum":0},
                        "id":{"type":"string","minLength":1,"maxLength":64},
                        "preset":{"type":"object"},
                        "text":{"type":"string","minLength":1,"maxLength":4096},
                        "character":{"type":"string"}
                    }))
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_tool_settings",
                "Change the application-owned ToolSettings through the GUI SettingsPresenter. Toggle enabled/calculator/memory_search/web_search/web_reader or set tool chain depth 1..5. Requires --allow-write. Inspect presenter_settings_read(area=tools) first; endpoints and credentials remain withheld.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["set_option","set_depth"]},
                        "option":{"type":"string","enum":["enabled","calculator","memory_search","web_search","web_reader"]},
                        "enabled":{"type":"boolean"},
                        "depth":{"type":"integer","minimum":1,"maximum":5}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_speech_capture",
                "Start or stop the same ASR capture owned by ApplicationLifecycle and used by GUI. On start supply selected character and selection_revision; optional absolute FFmpeg/FFplay paths for non-native backend. Requires --allow-write; actual ASR model must be ready.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["enabled"])),
                    ("properties".into(),json!({
                        "enabled":{"type":"boolean"},
                        "character":{"type":"string"},
                        "selection_revision":{"type":"integer","minimum":0},
                        "press_on_start":{"type":"boolean"},
                        "ffmpeg":{"type":"string"},
                        "ffplay":{"type":"string"}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            inspection_tool("presenter_speech_transcript",
                "Read the latest recognized transcript and routing identity emitted by the SAME application-owned transcript hub observed by GUI. Requires --allow-content for text.",
                &[],json!({"include_content":{"type":"boolean"}})),
            Tool::new("presenter_speech_session",
                "Send push-to-talk press/release into the running SpeechSessionController. Requires an active speech capture started by the host; does NOT start a new ASR worker/session.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["pressed"])),
                    ("properties".into(),json!({"pressed":{"type":"boolean"}})),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_speech_settings",
                "Select TTS/ASR model, save typed TTS/ASR preferences, assign per-character voice, or select ASR recognizer through the same SettingsPresenter as GUI. Changes require --allow-write. Read current values with presenter_settings_read first; saving TTS/ASR preserves GUI-owned selection and voice fields.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["select_model","save_tts","save_asr","select_asr_recognizer","save_character_voice"]},
                        "service":{"type":"string","enum":["tts","asr"]},
                        "model":{"type":["object","null"],"description":"SpeechModelReference {model_id,implementation,revision?}; null clears selection"},
                        "preferences":{"type":"object","description":"Complete TtsPreferences or AsrPreferences from presenter_settings_read; all fields required"},
                        "language":{"type":"string","maxLength":32},
                        "model_id":{"type":"string","maxLength":128},
                        "character":{"type":"string","description":"Canonical character ID; omitted for default voice"},
                        "voice_id":{"type":"string","maxLength":512}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            Tool::new("presenter_ai_hub_install",
                "Preview a verified Python AI Hub installation, then confirm the EXACT admitted token after the owner reports can_install=true. Uses GUI's installation coordinator; requires --allow-write.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action","category","id","token"])),
                    ("properties".into(),json!({
                        "action":{"type":"string","enum":["preview","confirm"]},
                        "category":{"type":"string","enum":["tts","asr"]},
                        "id":{"type":"string","minLength":1,"maxLength":256},
                        "token":{"type":"integer","minimum":0,"maximum":2147483647}
                    })),
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            inspection_tool("presenter_native_inventory",
                "Read exact native model revisions, selections and worker state from the same AI Hub service used by GUI.",
                &[],json!({"refresh":{"type":"boolean"}})),
            admin_write_tool("presenter_native",
                "Execute native AI Hub operations through the application's existing native service. Requires --allow-write.",
                &["action"],json!({
                    "action":{"type":"string","enum":["refresh","select","activate","stop","install","cancel_install"]},
                    "model_id":{"type":"string","maxLength":256},
                    "manifest_sha256":{"type":"string","minLength":64,"maxLength":64},
                    "revision":{"type":"string","minLength":64,"maxLength":64},
                    "backend":{"type":"string","enum":["cpu","cuda","wgpu","onnx"]},
                    "service":{"type":"string","enum":["tts","asr"]}
                })),
            inspection_tool("presenter_wait",
                "Wait for an actual Settings/Chat/CharacterMemory observer change (no polling). Timeout max 30s.",
                &["target"],json!({"target":{"type":"string","enum":["settings","chat","memory","native_download","ai_hub_install","speech_transcript","telegram_auth","remote_voice"]},
                    "character":{"type":"string"},"after_revision":{"type":"integer","minimum":0},
                    "after_version":{"type":"string","minLength":64,"maxLength":64},
                    "timeout_ms":{"type":"integer","minimum":1,"maximum":30000}})),
            inspection_tool("presenter_settings_read",
                "Read an allowlisted settings area from the actual SettingsPresenter. Credentials withheld.",
                &["area"], json!({"area":{"type":"string","enum":["interface","voice","memory","capture","onboarding","tts","asr","history_generation","document_generation","tools","persistence"]}})),
            inspection_tool("presenter_character_profile",
                "Read one character's actual persisted preset assignments and optional prompt; prompt requires --allow-content.",
                &["character"],json!({"character":{"type":"string"},"include_content":{"type":"boolean"}})),
            inspection_tool("presenter_memories",
                "Page character-owned memory records; content is opt-in. Returns a version token for guarded updates.",
                &["character"],json!({"character":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":100},"include_content":{"type":"boolean"},"state":{"type":"string","enum":["active","forgotten","deleted"]}})),
            inspection_tool("presenter_history",
                "Page visible ConversationPresenter history at a stable timeline version; content requires --allow-content.",
                &["character"],json!({"character":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":100},"include_content":{"type":"boolean"},"version":{"type":"object"}})),
            inspection_tool("presenter_conversation_preview",
                "Preview an edit against the visible conversation; does not mutate the Timeline.",
                &["character","change"],json!({"character":{"type":"string"},"change":{"type":"object"}})),
            inspection_tool("presenter_regenerate_preview",
                "Validate canonical reply provenance; does not regenerate or mutate anything.",
                &["character","event_id","version"],json!({"character":{"type":"string"},"event_id":{"type":"string","format":"uuid"},"version":{"type":"object"}})),
            admin_write_tool("presenter_settings_write",
                "Set a supported GUI settings property using its SettingsPresenter.",
                &["field","value"],json!({"field":{"type":"string","enum":["language","font_size","hide_private","structured_display","voice_volume","voice_in_launcher","voice_in_game","voice_name","memory_dedup_enabled","memory_dedup_threshold"]},"value":{}})),
            admin_write_tool("presenter_character_profile_save",
                "Compare-and-set a complete CharacterSettings profile (prompt and operation preset assignments).",
                &["character","expected_revision","profile"],json!({"character":{"type":"string"},"expected_revision":{"type":"integer","minimum":0},"profile":{"type":"object"}})),
            admin_write_tool("presenter_memory_edit",
                "Apply 1..32 character-owned memory changes against the exact version returned by presenter_memories.",
                &["character","expected_version","commands"],json!({"character":{"type":"string"},"expected_version":{"type":"string","minLength":64,"maxLength":64},"commands":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object"}}})),
            admin_write_tool("presenter_conversation_edit",
                "Apply a previously previewed conversation change with branch/head/generation CAS.",
                &["character","version","edit"],json!({"character":{"type":"string"},"version":{"type":"object"},"edit":{"type":"object"}})),
            admin_write_tool("presenter_retry_failed",
                "Re-admit a failed request only when its player input is still the latest visible message.",
                &["character","request_id"],json!({"character":{"type":"string"},"request_id":{"type":"string","format":"uuid"}})),
            inspection_tool("presenter_memory_recall",
                "Explicit forgotten-memory inspection through running HistoricalRecallApi. Requires --allow-content. Does not inject into generation.",
                &["character","memory_id","query_text"],json!({"character":{"type":"string"},
                "memory_id":{"type":"string","format":"uuid"},
                "query_text":{"type":"string","minLength":1,"maxLength":4096}})),
            Tool::new("presenter_graph",
                "Review or confirm GUI-equivalent GraphMaintenance through shared owner. Preview requires character, memory_id, mode; apply requires character, request_id; abort requires request_id. Confirm needs --allow-write and Core opt-in NEUROMITA_ENABLE_LOCAL_GRAPH_MAINTENANCE=1.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["action"])),
                    ("properties".into(),json!({"action":{"type":"string","enum":["preview","apply","abort"]},
                        "character":{"type":"string"},"memory_id":{"type":"string","format":"uuid"},
                        "mode":{"type":"string","enum":["clear","reindex"]},
                        "request_id":{"type":"string","format":"uuid"}}))
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            admin_write_tool("presenter_attach_file",
                "Attach a local file as one verified Timeline event, using same ConversationPresenter as GUI. Requires exact current version, --allow-write, --allow-content and Core NEUROMITA_ENABLE_LOCAL_FILE_ATTACH=1.",
                &["character","version","source","text"],json!({"character":{"type":"string"},
                    "version":{"type":"object"},"source":{"type":"string","maxLength":4096},
                    "text":{"type":"string","maxLength":65536}})),
            Tool::new("presenter_prompt_folder",
                "Locate or open selected character PromptCatalog folder on Core host. Locate requires --allow-content (exposes host path); open also requires --allow-write and NEUROMITA_ENABLE_LOCAL_HOST_ACTIONS=1.",
                serde_json::Map::from_iter([
                    ("type".into(),json!("object")),("additionalProperties".into(),json!(false)),
                    ("required".into(),json!(["character","action"])),
                    ("properties".into(),json!({"action":{"type":"string","enum":["locate","open"]},"character":{"type":"string"}}))
                ])
            ).with_annotations(ToolAnnotations::new().read_only(false)),
            admin_write_tool("presenter_guide_retry",
                "Retry pending failed Guide save in same GUI-owned persistence controller. Requires --allow-write.",
                &[],json!({})),
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
        if matches!(
            request.name.as_ref(),
            "presenter_memory_recall" | "presenter_attach_file" | "presenter_prompt_folder"
        ) && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "local content/path requires --allow-content",
                None,
            ));
        }
        if request.name == "presenter_graph"
            && args.get("action") == Some(&json!("apply"))
            && !self.allow_write
        {
            return Err(ErrorData::invalid_params(
                "graph apply requires --allow-write",
                None,
            ));
        }
        if request.name == "presenter_prompt_folder"
            && args.get("action") == Some(&json!("open"))
            && !self.allow_write
        {
            return Err(ErrorData::invalid_params(
                "folder open requires --allow-write",
                None,
            ));
        }
        if request.name == "presenter_speech_transcript"
            && args.get("include_content") == Some(&Value::Bool(true))
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "transcript text requires --allow-content",
                None,
            ));
        }
        if request.name == "presenter_conversation"
            && args.get("include_content") == Some(&Value::Bool(true))
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "include_content requires --allow-content",
                None,
            ));
        }
        if matches!(
            request.name.as_ref(),
            "presenter_memories" | "presenter_history" | "presenter_character_profile"
        ) && args.get("include_content") == Some(&Value::Bool(true))
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "include_content requires --allow-content",
                None,
            ));
        }
        if matches!(
            request.name.as_ref(),
            "presenter_settings_write"
                | "presenter_character_profile_save"
                | "presenter_memory_edit"
                | "presenter_conversation_edit"
                | "presenter_retry_failed"
                | "presenter_native"
                | "presenter_ai_hub_install"
                | "presenter_speech_settings"
                | "presenter_speech_session"
                | "presenter_speech_capture"
                | "presenter_tool_settings"
                | "presenter_telegram"
                | "presenter_remote_voice"
                | "presenter_attach_file"
                | "presenter_guide_retry"
        ) && !self.allow_write
        {
            return Err(ErrorData::invalid_params(
                "Presenter writes require --allow-write",
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
        if request.name == "presenter_presets"
            && args.get("api_key").is_some()
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "setting a credential through MCP requires --allow-content",
                None,
            ));
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
                    (step.get("command").and_then(Value::as_str) == Some("presenter_action")
                        && step.get("action").and_then(|a| a.get("api_key")).is_some())
                        || (step.get("command").and_then(Value::as_str)
                            == Some("presenter_conversation")
                            && step.get("include_content") == Some(&Value::Bool(true)))
                        || (step.get("command").and_then(Value::as_str)
                            == Some("presenter_admin_read")
                            && step.get("query").and_then(|q| q.get("include_content"))
                                == Some(&Value::Bool(true)))
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
                        Some(
                            "presenter_action"
                                | "presenter_conversation"
                                | "presenter_inspect"
                                | "presenter_admin_read"
                                | "presenter_admin_write"
                        )
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
                | "presenter_presets"
                | "presenter_speech_settings"
                | "presenter_speech_session"
                | "presenter_speech_capture"
                | "presenter_tool_settings"
                | "presenter_telegram"
                | "presenter_remote_voice"
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
        if request.name == "presenter_telegram"
            && matches!(
                args.get("action").and_then(Value::as_str),
                Some("connect" | "code" | "password")
            )
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "Telegram auth requires --allow-content",
                None,
            ));
        }
        if request.name == "presenter_remote_voice"
            && (args.get("action").and_then(Value::as_str) == Some("preview")
                || args.get("preset").and_then(|p| p.get("api_key")).is_some())
            && !self.allow_content
        {
            return Err(ErrorData::invalid_params(
                "Remote Voice preview/credentials require --allow-content",
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
                    | "presets"
                    | "ai_hub_install"
                    | "telegram_auth"
                    | "remote_voice"
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
        "presenter_wait" => {
            let target = string(&mut args, "target")?;
            let mut query = json!({"query":"wait","target":target});
            for key in ["character", "after_revision", "after_version", "timeout_ms"] {
                if let Some(value) = args.remove(key) {
                    query[key] = value;
                }
            }
            json!({"command":"presenter_admin_read","query":query})
        }
        "presenter_settings_read" => {
            let area = string(&mut args, "area")?;
            json!({"command":"presenter_admin_read","query":{"query":"settings","area":area}})
        }
        "presenter_character_profile" => {
            let character = string(&mut args, "character")?;
            let include_content = args.remove("include_content").unwrap_or(json!(false));
            json!({"command":"presenter_admin_read","query":{"query":"character_profile","character":character,"include_content":include_content}})
        }
        "presenter_memories" | "presenter_history" => {
            let character = string(&mut args, "character")?;
            let mut query = json!({"query":if name=="presenter_memories"{"memories"}else{"history"},"character":character});
            for key in ["offset", "limit", "include_content", "state", "version"] {
                if let Some(value) = args.remove(key) {
                    query[key] = value;
                }
            }
            json!({"command":"presenter_admin_read","query":query})
        }
        "presenter_conversation_preview" => {
            let character = string(&mut args, "character")?;
            let change = args
                .remove("change")
                .ok_or_else(|| ErrorData::invalid_params("missing change", None))?;
            json!({"command":"presenter_admin_read","query":{"query":"conversation_preview","character":character,"change":change}})
        }
        "presenter_regenerate_preview" => {
            let character = string(&mut args, "character")?;
            let event_id = uuid_field(&mut args, "event_id")?;
            let version = args
                .remove("version")
                .ok_or_else(|| ErrorData::invalid_params("missing version", None))?;
            json!({"command":"presenter_admin_read","query":{"query":"regenerate_preview","character":character,"event_id":event_id,"version":version}})
        }
        "presenter_settings_write" => {
            let field = string(&mut args, "field")?;
            let value = args
                .remove("value")
                .ok_or_else(|| ErrorData::invalid_params("missing value", None))?;
            json!({"command":"presenter_admin_write","change":{"change":"settings","field":field,"value":value}})
        }
        "presenter_character_profile_save" => {
            let character = string(&mut args, "character")?;
            let expected_revision = args
                .remove("expected_revision")
                .ok_or_else(|| ErrorData::invalid_params("missing expected_revision", None))?;
            let profile = args
                .remove("profile")
                .ok_or_else(|| ErrorData::invalid_params("missing profile", None))?;
            json!({"command":"presenter_admin_write","change":{"change":"character_profile","character":character,"expected_revision":expected_revision,"profile":profile}})
        }
        "presenter_memory_edit" => {
            let character = string(&mut args, "character")?;
            let expected_version = string(&mut args, "expected_version")?;
            let commands = args
                .remove("commands")
                .ok_or_else(|| ErrorData::invalid_params("missing commands", None))?;
            json!({"command":"presenter_admin_write","change":{"change":"memory","character":character,"expected_version":expected_version,"commands":commands}})
        }
        "presenter_conversation_edit" => {
            let character = string(&mut args, "character")?;
            let version = args
                .remove("version")
                .ok_or_else(|| ErrorData::invalid_params("missing version", None))?;
            let edit = args
                .remove("edit")
                .ok_or_else(|| ErrorData::invalid_params("missing edit", None))?;
            json!({"command":"presenter_admin_write","change":{"change":"conversation","character":character,"version":version,"edit":edit}})
        }
        "presenter_memory_recall" => {
            let character = string(&mut args, "character")?;
            let memory_id = uuid_field(&mut args, "memory_id")?;
            let query_text = string(&mut args, "query_text")?;
            if query_text.trim().is_empty() || query_text.len() > 4096 {
                return Err(ErrorData::invalid_params(
                    "query_text must contain 1..4096 characters",
                    None,
                ));
            }
            json!({"command":"presenter_admin_read","query":{"query":"memory_recall",
                "character":character,"memory_id":memory_id,"query_text":query_text}})
        }
        "presenter_graph" => {
            let action = string(&mut args, "action")?;
            match action.as_str() {
                "preview" => {
                    let character = string(&mut args, "character")?;
                    let memory_id = uuid_field(&mut args, "memory_id")?;
                    let mode = string(&mut args, "mode")?;
                    if !matches!(mode.as_str(), "clear" | "reindex") {
                        return Err(ErrorData::invalid_params("invalid mode", None));
                    }
                    json!({"command":"presenter_admin_read","query":{"query":"graph_preview",
                        "character":character,"memory_id":memory_id,"mode":mode}})
                }
                "apply" => {
                    let character = string(&mut args, "character")?;
                    let request_id = uuid_field(&mut args, "request_id")?;
                    json!({"command":"presenter_admin_write","change":{"change":"graph_apply","character":character,"request_id":request_id}})
                }
                "abort" => {
                    let request_id = uuid_field(&mut args, "request_id")?;
                    json!({"command":"presenter_admin_write","change":{"change":"graph_abort","request_id":request_id}})
                }
                _ => return Err(ErrorData::invalid_params("invalid graph action", None)),
            }
        }
        "presenter_attach_file" => {
            let character = string(&mut args, "character")?;
            let source = string(&mut args, "source")?;
            let text = string(&mut args, "text")?;
            let version = args
                .remove("version")
                .filter(Value::is_object)
                .ok_or_else(|| ErrorData::invalid_params("version must be an object", None))?;
            if text.len() > 65536
                || source.len() > 4096
                || !std::path::Path::new(&source).is_absolute()
            {
                return Err(ErrorData::invalid_params(
                    "invalid attachment source/text",
                    None,
                ));
            }
            json!({"command":"presenter_admin_write","change":{"change":"attach_file",
                "character":character,"version":version,"text":text,"source":source}})
        }
        "presenter_prompt_folder" => {
            let character = string(&mut args, "character")?;
            let action = string(&mut args, "action")?;
            match action.as_str() {
                "locate" => {
                    json!({"command":"presenter_admin_read","query":{"query":"prompt_folder","character":character}})
                }
                "open" => {
                    json!({"command":"presenter_admin_write","change":{"change":"prompt_folder_open","character":character}})
                }
                _ => {
                    return Err(ErrorData::invalid_params(
                        "invalid prompt folder action",
                        None,
                    ));
                }
            }
        }
        "presenter_guide_retry" => {
            json!({"command":"presenter_admin_write","change":{"change":"guide_retry"}})
        }
        "presenter_retry_failed" => {
            let character = string(&mut args, "character")?;
            let request_id = uuid_field(&mut args, "request_id")?;
            json!({"command":"presenter_admin_write","change":{"change":"retry_failed","character":character,"request_id":request_id}})
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
        "presenter_telegram" => {
            let action = string(&mut args, "action")?;
            let command = match action.as_str() {
                "connect" => {
                    let api_id = string(&mut args, "api_id")?;
                    let api_hash = string(&mut args, "api_hash")?;
                    let phone = string(&mut args, "phone")?;
                    if api_id.len() > 32
                        || api_hash.is_empty()
                        || api_hash.len() > 128
                        || phone.is_empty()
                        || phone.len() > 64
                    {
                        return Err(ErrorData::invalid_params(
                            "invalid Telegram auth input",
                            None,
                        ));
                    }
                    json!({"action":"telegram_connect","api_id":api_id,"api_hash":api_hash,"phone":phone})
                }
                "code" => {
                    let code = string(&mut args, "code")?;
                    if code.is_empty() || code.len() > 32 {
                        return Err(ErrorData::invalid_params("invalid code", None));
                    }
                    json!({"action":"telegram_code","code":code})
                }
                "password" => {
                    let password = string(&mut args, "password")?;
                    if password.is_empty() || password.len() > 1024 {
                        return Err(ErrorData::invalid_params("invalid password", None));
                    }
                    json!({"action":"telegram_password","password":password})
                }
                "disconnect" => json!({"action":"telegram_disconnect"}),
                "refresh_voice" => json!({"action":"telegram_refresh_voice"}),
                _ => return Err(ErrorData::invalid_params("unknown Telegram action", None)),
            };
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "unexpected Telegram fields",
                    None,
                ));
            }
            json!({"command":"presenter_action","action":command})
        }
        "presenter_remote_voice" => {
            let action = string(&mut args, "action")?;
            let command = match action.as_str() {
                "cancel" => json!({"action":"remote_voice_cancel"}),
                "add" | "delete" | "select" | "select_with_save" | "save" | "preview" => {
                    let revision = args
                        .remove("expected_revision")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            ErrorData::invalid_params("expected_revision is required", None)
                        })?;
                    let mut obj = json!({"action":format!("remote_voice_{action}"),"expected_revision":revision});
                    if action == "select" || action == "select_with_save" {
                        obj["id"] = json!(string(&mut args, "id")?);
                    }
                    if action == "save" || action == "preview" || action == "select_with_save" {
                        let preset = args.remove("preset").ok_or_else(|| {
                            ErrorData::invalid_params("preset patch required", None)
                        })?;
                        if !preset.is_object() || !preset.get("id").is_some_and(Value::is_string) {
                            return Err(ErrorData::invalid_params(
                                "preset patch must have ID",
                                None,
                            ));
                        }
                        obj["preset"] = preset;
                    }
                    if action == "preview" {
                        let text = string(&mut args, "text")?;
                        if text.is_empty() || text.len() > 4096 {
                            return Err(ErrorData::invalid_params(
                                "preview text length must be 1..4096",
                                None,
                            ));
                        }
                        obj["text"] = json!(text);
                        if let Some(character) = args.remove("character") {
                            if !character.is_string() {
                                return Err(ErrorData::invalid_params("invalid character", None));
                            }
                            obj["character"] = character;
                        }
                    }
                    obj
                }
                _ => {
                    return Err(ErrorData::invalid_params(
                        "unknown Remote Voice action",
                        None,
                    ));
                }
            };
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "unexpected Remote Voice fields",
                    None,
                ));
            }
            json!({"command":"presenter_action","action":command})
        }
        "presenter_tool_settings" => {
            let action = string(&mut args, "action")?;
            let payload = match action.as_str() {
                "set_option" => {
                    let option = string(&mut args, "option")?;
                    if !matches!(
                        option.as_str(),
                        "enabled" | "calculator" | "memory_search" | "web_search" | "web_reader"
                    ) {
                        return Err(ErrorData::invalid_params("unknown tool option", None));
                    }
                    let enabled = args
                        .remove("enabled")
                        .and_then(|v| v.as_bool())
                        .ok_or_else(|| {
                            ErrorData::invalid_params("enabled must be boolean", None)
                        })?;
                    json!({"action":"tool_set_option","option":option,"enabled":enabled})
                }
                "set_depth" => {
                    let depth = args
                        .remove("depth")
                        .and_then(|v| v.as_u64())
                        .filter(|v| (1..=5).contains(v))
                        .ok_or_else(|| ErrorData::invalid_params("depth must be 1..5", None))?;
                    json!({"action":"tool_set_depth","depth":depth})
                }
                _ => {
                    return Err(ErrorData::invalid_params(
                        "unknown tool settings action",
                        None,
                    ));
                }
            };
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "unexpected tool settings fields",
                    None,
                ));
            }
            json!({"command":"presenter_action","action":payload})
        }
        "presenter_speech_transcript" => {
            let include_content = args.remove("include_content").unwrap_or(json!(false));
            if !include_content.is_boolean() || !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "invalid transcript read options",
                    None,
                ));
            }
            json!({"command":"presenter_admin_read","query":{
                "query":"speech_transcript","include_content":include_content}})
        }
        "presenter_speech_capture" => {
            let enabled = args
                .remove("enabled")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| ErrorData::invalid_params("enabled must be bool", None))?;
            let mut action = json!({"action":"speech_capture","enabled":enabled});
            for key in [
                "character",
                "selection_revision",
                "press_on_start",
                "ffmpeg",
                "ffplay",
            ] {
                if let Some(value) = args.remove(key) {
                    action[key] = value;
                }
            }
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "unexpected speech capture options",
                    None,
                ));
            }
            if enabled
                && (!action["character"].is_string() || !action["selection_revision"].is_u64())
            {
                return Err(ErrorData::invalid_params(
                    "start requires character and selection_revision",
                    None,
                ));
            }
            json!({"command":"presenter_action","action":action})
        }
        "presenter_speech_inventory" => {
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "speech inventory takes no arguments",
                    None,
                ));
            }
            json!({"command":"presenter_admin_read","query":{"query":"speech_inventory"}})
        }
        "presenter_speech_session" => {
            let pressed = args
                .remove("pressed")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| ErrorData::invalid_params("pressed must be boolean", None))?;
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "unexpected speech session fields",
                    None,
                ));
            }
            json!({"command":"presenter_action","action":{"action":"speech_push_to_talk","pressed":pressed}})
        }
        "presenter_speech_settings" => {
            let action = string(&mut args, "action")?;
            let mut payload = json!({"action":format!("speech_{action}")});
            match action.as_str() {
                "select_model" => {
                    let service = string(&mut args, "service")?;
                    if !matches!(service.as_str(), "tts" | "asr") {
                        return Err(ErrorData::invalid_params(
                            "service must be tts or asr",
                            None,
                        ));
                    }
                    payload["service"] = json!(service);
                    payload["model"] = args.remove("model").ok_or_else(|| {
                        ErrorData::invalid_params(
                            "missing model (use null to clear selection)",
                            None,
                        )
                    })?;
                }
                "save_tts" | "save_asr" => {
                    let preferences = args
                        .remove("preferences")
                        .filter(Value::is_object)
                        .ok_or_else(|| {
                            ErrorData::invalid_params("preferences must be object", None)
                        })?;
                    payload["preferences"] = preferences;
                    if action == "save_asr" {
                        if let Some(language) = args.remove("language") {
                            if !language.as_str().is_some_and(|s| s.len() <= 32) {
                                return Err(ErrorData::invalid_params("invalid language", None));
                            }
                            payload["language"] = language;
                        }
                    }
                }
                "select_asr_recognizer" => {
                    let model_id = string(&mut args, "model_id")?;
                    if model_id.len() > 128 {
                        return Err(ErrorData::invalid_params("model_id too long", None));
                    }
                    payload["model_id"] = json!(model_id);
                }
                "save_character_voice" => {
                    let voice_id = string(&mut args, "voice_id")?;
                    if voice_id.len() > 512 {
                        return Err(ErrorData::invalid_params("voice_id too long", None));
                    }
                    payload["voice_id"] = json!(voice_id);
                    if let Some(character) = args.remove("character") {
                        payload["character"] = character;
                    }
                }
                _ => {
                    return Err(ErrorData::invalid_params(
                        "unknown speech settings action",
                        None,
                    ));
                }
            }
            if !args.is_empty() {
                return Err(ErrorData::invalid_params(
                    "unexpected speech settings fields",
                    None,
                ));
            }
            json!({"command":"presenter_action","action":payload})
        }
        "presenter_ai_hub_install" => {
            let action = string(&mut args, "action")?;
            if !matches!(action.as_str(), "preview" | "confirm") {
                return Err(ErrorData::invalid_params(
                    "unknown AI Hub install action",
                    None,
                ));
            }
            let category = string(&mut args, "category")?;
            if !matches!(category.as_str(), "tts" | "asr") {
                return Err(ErrorData::invalid_params(
                    "AI Hub category must be tts or asr",
                    None,
                ));
            }
            let id = string(&mut args, "id")?;
            if id.is_empty() || id.len() > 256 || !id.is_ascii() {
                return Err(ErrorData::invalid_params("invalid AI Hub model ID", None));
            }
            let token = args
                .remove("token")
                .and_then(|v| v.as_i64())
                .filter(|n| (0..=i32::MAX as i64).contains(n))
                .ok_or_else(|| ErrorData::invalid_params("token must be nonnegative i32", None))?;
            json!({"command":"presenter_action","action":{
                "action":format!("ai_hub_{action}_install"),"category":category,"id":id,"token":token
            }})
        }
        "presenter_native_inventory" => {
            let refresh = args.remove("refresh").map_or(Ok(false), |v| {
                v.as_bool()
                    .ok_or_else(|| ErrorData::invalid_params("refresh must be boolean", None))
            })?;
            json!({"command":"presenter_admin_read","query":{"query":"native_models","refresh":refresh}})
        }
        "presenter_native" => {
            let action = string(&mut args, "action")?;
            let mut payload = json!({"action":format!("native_{action}")});
            let required: &[&str] = match action.as_str() {
                "refresh" | "cancel_install" => &[],
                "select" | "install" => &["model_id", "manifest_sha256", "backend"],
                "activate" | "stop" => &["model_id", "service", "revision"],
                _ => return Err(ErrorData::invalid_params("unknown native operation", None)),
            };
            for key in required {
                payload[*key] = args
                    .remove(*key)
                    .ok_or_else(|| ErrorData::invalid_params(format!("missing {key}"), None))?;
            }
            json!({"command":"presenter_action","action":payload})
        }
        "presenter_presets" => {
            let action = string(&mut args, "action")?;
            let mut payload = json!({"action":format!("preset_{action}")});
            match action.as_str() {
                "create_unconfigured" => {}
                "select" | "move" | "delete" | "update_connection" => {
                    let id = args
                        .remove("preset_id")
                        .and_then(|v| v.as_u64())
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| ErrorData::invalid_params("preset_id must be u32", None))?;
                    payload["preset_id"] = json!(id);
                    if action == "move" {
                        let step = args
                            .remove("step")
                            .and_then(|v| v.as_i64())
                            .filter(|v| matches!(v, -1 | 1))
                            .ok_or_else(|| {
                                ErrorData::invalid_params("step must be -1 or 1", None)
                            })?;
                        payload["step"] = json!(step);
                    }
                    if action == "update_connection" {
                        for name in [
                            "expected_revision",
                            "name",
                            "template_id",
                            "endpoint",
                            "model",
                            "temperature",
                            "max_output_tokens",
                            "timeout_ms",
                            "stream",
                            "max_retries",
                            "send_temperature",
                            "send_max_output_tokens",
                        ] {
                            let value = args.remove(name).ok_or_else(|| {
                                ErrorData::invalid_params(format!("missing {name}"), None)
                            })?;
                            payload[name] = value;
                        }
                        if let Some(key) = args.remove("api_key") {
                            payload["api_key"] = key;
                        }
                    }
                }
                _ => {
                    return Err(ErrorData::invalid_params(
                        "unknown presenter preset action",
                        None,
                    ));
                }
            }
            json!({"command":"presenter_action","action":payload})
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

fn admin_write_tool(
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
    Tool::new(name, description, schema).with_annotations(ToolAnnotations::new().read_only(false))
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
    fn presenter_admin_tools_are_typed_and_fail_closed() {
        let read = command_for_tool(
            "presenter_memories",
            args(json!({"character":"Crazy","limit":20})),
        )
        .unwrap();
        assert_eq!(read["command"], "presenter_admin_read");
        assert_eq!(read["query"]["query"], "memories");
        assert_eq!(read["query"]["limit"], 20);
        let edit=command_for_tool("presenter_memory_edit",args(json!({"character":"Crazy",
            "expected_version":"a".repeat(64),"commands":[{"command":"forget","id":Uuid::new_v4()}]}))).unwrap();
        assert_eq!(edit["command"], "presenter_admin_write");
        assert_eq!(edit["change"]["change"], "memory");
        assert!(
            command_for_tool(
                "presenter_settings_read",
                args(json!({"area":"voice","password":"bad"}))
            )
            .is_err()
        );
        assert!(
            command_for_tool(
                "presenter_conversation_edit",
                args(json!({"character":"Crazy"}))
            )
            .is_err()
        );
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
        let update = command_for_tool("presenter_presets", args(json!({
            "action":"update_connection", "preset_id":42, "expected_revision":7,
            "name":"Demo", "template_id":"openai", "endpoint":"https://api.openai.com/v1/responses",
            "model":"gpt-test", "temperature":0.7, "max_output_tokens":1000,
            "timeout_ms":60000, "stream":true, "max_retries":2,
            "send_temperature":true, "send_max_output_tokens":true
        }))).unwrap();
        assert_eq!(update["action"]["action"], "preset_update_connection");
        assert_eq!(update["action"]["expected_revision"], 7);
        assert!(update["action"].get("api_key").is_none());
        assert!(command_for_tool("inspect_presenter", args(json!({"surface":"presets"}))).is_ok());
        let preview = command_for_tool(
            "presenter_ai_hub_install",
            args(json!({"action":"preview","category":"tts","id":"demo","token":19})),
        )
        .unwrap();
        assert_eq!(preview["action"]["action"], "ai_hub_preview_install");
        assert_eq!(preview["action"]["token"], 19);
        let confirm = command_for_tool(
            "presenter_ai_hub_install",
            args(json!({"action":"confirm","category":"asr","id":"demo","token":19})),
        )
        .unwrap();
        assert_eq!(confirm["action"]["action"], "ai_hub_confirm_install");
        assert!(
            command_for_tool(
                "inspect_presenter",
                args(json!({"surface":"ai_hub_install"}))
            )
            .is_ok()
        );
        for payload in [
            json!({"action":"confirm","category":"asr","id":"demo","token":-1}),
            json!({"action":"preview","category":"other","id":"demo","token":5}),
            json!({"action":"preview","category":"tts","id":"","token":5}),
            json!({"action":"confirm","category":"tts","id":"demo","token":5,"shell":"rm -rf /"}),
        ] {
            assert!(command_for_tool("presenter_ai_hub_install", args(payload)).is_err());
        }
        let inventory =
            command_for_tool("presenter_native_inventory", args(json!({"refresh":true}))).unwrap();
        assert_eq!(inventory["command"], "presenter_admin_read");
        assert_eq!(inventory["query"]["query"], "native_models");
        assert_eq!(inventory["query"]["refresh"], true);
        for action in ["refresh", "cancel_install"] {
            let command =
                command_for_tool("presenter_native", args(json!({"action":action}))).unwrap();
            assert_eq!(command["action"]["action"], format!("native_{action}"));
        }
        for action in ["select", "install"] {
            let command=command_for_tool("presenter_native",args(json!({
                "action":action,"model_id":"voice","manifest_sha256":"a".repeat(64),"backend":"cuda"
            }))).unwrap();
            assert_eq!(command["action"]["backend"], "cuda");
        }
        for action in ["activate", "stop"] {
            let command = command_for_tool(
                "presenter_native",
                args(json!({
                    "action":action,"model_id":"voice","revision":"a".repeat(64),"service":"tts"
                })),
            )
            .unwrap();
            assert_eq!(command["action"]["service"], "tts");
        }
        assert!(
            command_for_tool(
                "presenter_native",
                args(json!({"action":"activate", "model_id":"voice"}))
            )
            .is_err()
        );
        assert!(command_for_tool("presenter_native", args(json!({"action":"invalid"}))).is_err());
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

#[cfg(test)]
mod speech_admin_tests {
    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn speech_settings_tool_maps_typed_actions_and_rejects_unknown_fields() {
        let asr=command_for_tool("presenter_speech_settings",args(json!({
            "action":"select_model","service":"asr","model":{"model_id":"demo_asr","implementation":"python"}
        }))).unwrap();
        assert_eq!(asr["action"]["action"], "speech_select_model");
        assert_eq!(asr["action"]["model"]["model_id"], "demo_asr");
        let clear = command_for_tool(
            "presenter_speech_settings",
            args(json!({
                "action":"select_model","service":"tts","model":null
            })),
        )
        .unwrap();
        assert!(clear["action"]["model"].is_null());
        let voice = command_for_tool(
            "presenter_speech_settings",
            args(json!({
                "action":"save_character_voice","character":"Crazy","voice_id":"kind-01"
            })),
        )
        .unwrap();
        assert_eq!(voice["action"]["action"], "speech_save_character_voice");
        let tools = command_for_tool(
            "presenter_tool_settings",
            args(json!({"action":"set_option","option":"calculator","enabled":true})),
        )
        .unwrap();
        assert_eq!(tools["action"]["action"], "tool_set_option");
        assert_eq!(tools["action"]["option"], "calculator");
        let depth = command_for_tool(
            "presenter_tool_settings",
            args(json!({"action":"set_depth","depth":4})),
        )
        .unwrap();
        assert_eq!(depth["action"]["depth"], 4);
        for invalid in [
            json!({"action":"set_option","option":"unknown","enabled":true}),
            json!({"action":"set_depth","depth":0}),
            json!({"action":"set_depth","depth":6}),
            json!({"action":"set_option","option":"calculator","enabled":true,"credential":"x"}),
        ] {
            assert!(command_for_tool("presenter_tool_settings", args(invalid)).is_err());
        }
        let inventory = command_for_tool("presenter_speech_inventory", args(json!({}))).unwrap();
        assert_eq!(inventory["query"]["query"], "speech_inventory");
        let press =
            command_for_tool("presenter_speech_session", args(json!({"pressed":true}))).unwrap();
        assert_eq!(press["action"]["action"], "speech_push_to_talk");
        assert_eq!(press["action"]["pressed"], true);
        assert!(
            command_for_tool("presenter_speech_session", args(json!({"pressed":"yes"}))).is_err()
        );
        assert!(
            command_for_tool(
                "presenter_speech_inventory",
                args(json!({"dump_paths":true}))
            )
            .is_err()
        );
        for invalid in [
            json!({"action":"select_model","service":"embedding","model":null}),
            json!({"action":"select_model","service":"tts"}),
            json!({"action":"save_tts","preferences":"not an object"}),
            json!({"action":"save_asr","preferences":{},"language":true}),
            json!({"action":"select_asr_recognizer","model_id":"x","shell":"rm -rf /"}),
            json!({"action":"unknown"}),
        ] {
            assert!(command_for_tool("presenter_speech_settings", args(invalid)).is_err());
        }
    }
}

#[cfg(test)]
mod speech_capture_admin_tests {
    use super::*;
    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn maps_capture_and_transcript_to_typed_owner_commands() {
        let start = command_for_tool(
            "presenter_speech_capture",
            args(json!({
                "enabled":true,"character":"Crazy","selection_revision":4,"press_on_start":true
            })),
        )
        .unwrap();
        assert_eq!(start["command"], "presenter_action");
        assert_eq!(start["action"]["action"], "speech_capture");
        assert_eq!(start["action"]["selection_revision"], 4);
        assert_eq!(start["action"]["press_on_start"], true);
        let stop =
            command_for_tool("presenter_speech_capture", args(json!({"enabled":false}))).unwrap();
        assert_eq!(stop["action"]["enabled"], false);
        let read = command_for_tool(
            "presenter_speech_transcript",
            args(json!({"include_content":false})),
        )
        .unwrap();
        assert_eq!(read["query"]["query"], "speech_transcript");
        assert!(
            command_for_tool("presenter_speech_capture", args(json!({"enabled":true}))).is_err()
        );
        assert!(
            command_for_tool("presenter_speech_capture", args(json!({"enabled":"yes"}))).is_err()
        );
        assert!(command_for_tool("presenter_speech_capture",args(json!({"enabled":true,"character":"Crazy","selection_revision":2,"shell":"exec"}))).is_err());
        assert!(
            command_for_tool(
                "presenter_speech_transcript",
                args(json!({"include_content":"yes"}))
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod telegram_admin_tests {
    use super::*;
    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }
    #[test]
    fn auth_payloads_are_typed_bounded_and_never_echoed_in_mcp_metadata() {
        for (action, extra) in [
            (
                "connect",
                json!({"api_id":"1234","api_hash":"secret","phone":"+123456789"}),
            ),
            ("code", json!({"code":"456789"})),
            ("password", json!({"password":"private password"})),
            ("disconnect", json!({})),
            ("refresh_voice", json!({})),
        ] {
            let mut object = extra.as_object().unwrap().clone();
            object.insert("action".into(), json!(action));
            let cmd = command_for_tool("presenter_telegram", object).unwrap();
            assert_eq!(cmd["action"]["action"], format!("telegram_{action}"));
            assert_eq!(cmd["command"], "presenter_action");
        }
        for bad in [
            json!({"action":"code","code":""}),
            json!({"action":"connect","api_id":"1"}),
            json!({"action":"password","password":"x","debug":true}),
            json!({"action":"unknown"}),
        ] {
            assert!(command_for_tool("presenter_telegram", args(bad)).is_err());
        }
        assert!(
            command_for_tool(
                "inspect_presenter",
                args(json!({"surface":"telegram_auth"}))
            )
            .is_ok()
        );
    }
}

#[cfg(test)]
mod remote_voice_tool_tests {
    use super::*;
    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn remote_voice_tools_expose_safe_revision_fenced_actions() {
        let cmd = command_for_tool(
            "presenter_remote_voice",
            args(json!({"action":"save", "expected_revision":7,
            "preset":{"id":"fish_audio_default","name":"Custom","speed_percent":120}})),
        )
        .unwrap();
        assert_eq!(cmd["command"], "presenter_action");
        assert_eq!(cmd["action"]["action"], "remote_voice_save");
        assert_eq!(cmd["action"]["expected_revision"], 7);
        assert!(
            command_for_tool(
                "presenter_remote_voice",
                args(json!({"action":"save",
            "preset":{"id":"fish_audio_default"}}))
            )
            .is_err()
        );
        assert!(
            command_for_tool(
                "presenter_remote_voice",
                args(json!({"action":"preview", "expected_revision":1,
            "preset":{"id":"fish_audio_default"}, "text":""}))
            )
            .is_err()
        );
        assert!(
            command_for_tool(
                "presenter_remote_voice",
                args(json!({"action":"cancel", "api_key":"leak"}))
            )
            .is_err()
        );
        let inspect =
            command_for_tool("inspect_presenter", args(json!({"surface":"remote_voice"}))).unwrap();
        assert_eq!(inspect["surface"], "remote_voice");
        let wait = command_for_tool(
            "presenter_wait",
            args(json!({"target":"remote_voice","after_revision":4})),
        )
        .unwrap();
        assert_eq!(wait["query"]["target"], "remote_voice");
    }
}

#[cfg(test)]
mod cycle16_gui_parity_tests {
    use super::*;
    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }
    #[test]
    fn typed_gui_commands_map_to_presenter_owners() {
        let uuid = Uuid::new_v4();
        let graph = command_for_tool(
            "presenter_graph",
            args(json!({"action":"preview","character":"Crazy","memory_id":uuid,"mode":"clear"})),
        )
        .unwrap();
        assert_eq!(graph["query"]["query"], "graph_preview");
        let apply = command_for_tool(
            "presenter_graph",
            args(json!({"action":"apply","character":"Crazy","request_id":uuid})),
        )
        .unwrap();
        assert_eq!(apply["change"]["change"], "graph_apply");
        let abort = command_for_tool(
            "presenter_graph",
            args(json!({"action":"abort","request_id":uuid})),
        )
        .unwrap();
        assert_eq!(abort["change"]["change"], "graph_abort");
        let recall = command_for_tool(
            "presenter_memory_recall",
            args(json!({"character":"Crazy","memory_id":uuid,"query_text":"find"})),
        )
        .unwrap();
        assert_eq!(recall["query"]["query"], "memory_recall");
        let file = command_for_tool(
            "presenter_attach_file",
            args(json!({"character":"Crazy","version":{},"text":"x","source":"/tmp/local.txt"})),
        )
        .unwrap();
        assert_eq!(file["change"]["change"], "attach_file");
        let folder = command_for_tool(
            "presenter_prompt_folder",
            args(json!({"action":"open","character":"Crazy"})),
        )
        .unwrap();
        assert_eq!(folder["change"]["change"], "prompt_folder_open");
        let guide = command_for_tool("presenter_guide_retry", args(json!({}))).unwrap();
        assert_eq!(guide["change"]["change"], "guide_retry");
        for invalid in [
            json!({"action":"preview","character":"Crazy","memory_id":uuid,"mode":"drop"}),
            json!({"action":"apply","character":"Crazy","request_id":"abc"}),
            json!({"action":"abort","request_id":uuid,"unused":true}),
        ] {
            assert!(command_for_tool("presenter_graph", args(invalid)).is_err());
        }
        assert!(
            command_for_tool(
                "presenter_memory_recall",
                args(json!({"character":"Crazy","memory_id":uuid,"query_text":""}))
            )
            .is_err()
        );
        assert!(
            command_for_tool(
                "presenter_attach_file",
                args(json!({"character":"Crazy","version":{},"text":"x","source":"relative"}))
            )
            .is_err()
        );
    }
}
