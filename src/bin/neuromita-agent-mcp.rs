use std::{net::SocketAddr, time::Duration};

use neuromita::agent_ipc::call_local_control;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
    transport::stdio,
};
use serde_json::{Value, json};

struct NeuroMita {
    core: SocketAddr,
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
                "Read a diagnostic snapshot from the running NeuroMita Core.",
                "scope",
                &[
                    "capabilities",
                    "lifecycle",
                    "timeline",
                    "character",
                    "requests",
                ],
            ),
            inspection_tool(
                "inspect_presenter",
                "Read a presenter snapshot. Chat requires a canonical character ID.",
                "surface",
                &["chat", "ai_runtime", "connections"],
            ),
        ]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let mut args = request.arguments.unwrap_or_default();
        let (method, selector) = match request.name.as_ref() {
            "inspect_core" => ("debug_inspect", "scope"),
            "inspect_presenter" => ("presenter_inspect", "surface"),
            _ => return Err(ErrorData::invalid_params("unknown NeuroMita tool", None)),
        };
        let choice = args.remove(selector).and_then(|value| match value {
            Value::String(value) => Some(value),
            _ => None,
        });
        let Some(choice) = choice else {
            return Err(ErrorData::invalid_params(
                format!("missing {selector}"),
                None,
            ));
        };
        let character = args.remove("character");
        if !args.is_empty() || character.as_ref().is_some_and(|id| !id.is_string()) {
            return Err(ErrorData::invalid_params("invalid tool arguments", None));
        }
        let mut command = json!({"command": method});
        command[selector] = json!(choice);
        if let Some(character) = character {
            command["character"] = character;
        }
        let response = match call_local_control(self.core, Duration::from_secs(10), command).await {
            Ok(response) if response["type"] != "error" => CallToolResult::structured(response),
            Ok(response) => CallToolResult::structured_error(json!({"code": response["code"]})),
            Err(error) => CallToolResult::structured_error(json!({"error": error.to_string()})),
        };
        Ok(response.into())
    }
}

fn inspection_tool(
    name: &'static str,
    description: &'static str,
    selector: &str,
    choices: &[&str],
) -> Tool {
    let schema = serde_json::Map::from_iter([
        ("type".into(), json!("object")),
        ("additionalProperties".into(), json!(false)),
        ("required".into(), json!([selector])),
        (
            "properties".into(),
            json!({
                (selector): {"type": "string", "enum": choices},
                "character": {"type": "string", "description": "Canonical character ID"}
            }),
        ),
    ]);
    Tool::new(name, description, schema).with_annotations(ToolAnnotations::new().read_only(true))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let core: SocketAddr = match args.as_slice() {
        [flag, address] if flag == "--core" => address.parse()?,
        _ => return Err("usage: neuromita-agent-mcp --core LOOPBACK:PORT".into()),
    };
    if !core.ip().is_loopback() {
        return Err("LocalControl address must be loopback".into());
    }
    NeuroMita { core }.serve(stdio()).await?.waiting().await?;
    Ok(())
}
