use std::{env, io, net::SocketAddr, sync::Arc, time::Duration};

use neuromita::agent_ipc::call_local_control;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use uuid::Uuid;

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(6);

struct Options {
    listen: SocketAddr,
    core: SocketAddr,
    allow_write: bool,
    token: Arc<str>,
}

fn config() -> io::Result<Options> {
    let mut args = env::args().skip(1);
    let mut listen: SocketAddr = "127.0.0.1:8766".parse().unwrap();
    let mut core: SocketAddr = "127.0.0.1:8765".parse().unwrap();
    let mut allow_write = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => {
                listen = args
                    .next()
                    .ok_or_else(|| invalid("missing --listen address"))?
                    .parse()
                    .map_err(|_| invalid("invalid --listen address"))?
            }
            "--core" => {
                core = args
                    .next()
                    .ok_or_else(|| invalid("missing --core address"))?
                    .parse()
                    .map_err(|_| invalid("invalid --core address"))?
            }
            "--allow-write" => allow_write = true,
            _ => {
                return Err(invalid(
                    "usage: neuromita-agent-http [--listen LOOPBACK:PORT] [--core LOOPBACK:PORT] [--allow-write]",
                ));
            }
        }
    }
    if !listen.ip().is_loopback() || !core.ip().is_loopback() {
        return Err(invalid("both sockets must be loopback"));
    }
    let token = env::var("NEUROMITA_AGENT_HTTP_TOKEN")
        .map_err(|_| invalid("NEUROMITA_AGENT_HTTP_TOKEN is required"))?;
    if token.len() < 32 || !token.is_ascii() {
        return Err(invalid("HTTP token must be at least 32 ASCII bytes"));
    }
    Ok(Options {
        listen,
        core,
        allow_write,
        token: token.into(),
    })
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

struct HttpRequest {
    method: String,
    path: String,
    auth: String,
    host: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut raw = Vec::with_capacity(2048);
    let header_end = loop {
        if let Some(pos) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break pos;
        }
        if raw.len() > MAX_HEADERS {
            return Err(invalid("headers too large"));
        }
        let mut buf = [0u8; 2048];
        let count = stream.read(&mut buf).await?;
        if count == 0 {
            return Err(invalid("incomplete headers"));
        }
        raw.extend_from_slice(&buf[..count]);
    };
    if header_end > MAX_HEADERS {
        return Err(invalid("headers too large"));
    }
    let header =
        std::str::from_utf8(&raw[..header_end]).map_err(|_| invalid("non-UTF8 headers"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| invalid("missing request line"))?;
    let parts = request_line.split_ascii_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        return Err(invalid("HTTP/1.1 required"));
    }
    let (method, path) = (parts[0].to_owned(), parts[1].to_owned());
    if path.len() > 128 || !path.is_ascii() || path.contains('?') {
        return Err(invalid("unsupported path"));
    }
    let mut host = None;
    let mut auth = None;
    let mut length = None;
    let mut content_type = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid header"))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        match name.as_str() {
            "host" => {
                if host.replace(value).is_some() {
                    return Err(invalid("duplicate Host"));
                }
            }
            "authorization" => {
                if auth.replace(value).is_some() {
                    return Err(invalid("duplicate Authorization"));
                }
            }
            "content-length" => {
                if length.replace(value).is_some() {
                    return Err(invalid("duplicate Content-Length"));
                }
            }
            "content-type" => {
                if content_type.replace(value).is_some() {
                    return Err(invalid("duplicate Content-Type"));
                }
            }
            "transfer-encoding" => return Err(invalid("chunked HTTP is not supported")),
            _ => {}
        }
    }
    let length: usize = match length {
        Some(value) => value
            .parse()
            .map_err(|_| invalid("invalid Content-Length"))?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(invalid("body too large"));
    }
    if method == "POST" && (length == 0 || content_type.as_deref() != Some("application/json")) {
        return Err(invalid("POST requires application/json and Content-Length"));
    }
    if method != "POST" && length != 0 {
        return Err(invalid("GET cannot include a body"));
    }
    let mut body = raw[header_end + 4..].to_vec();
    if body.len() > length {
        return Err(invalid("HTTP pipelining is not supported"));
    }
    body.resize(length, 0);
    stream
        .read_exact(&mut body[raw.len() - header_end - 4..])
        .await?;
    Ok(HttpRequest {
        method,
        path,
        auth: auth.unwrap_or_default(),
        host: host.unwrap_or_default(),
        body,
    })
}

fn authorized(received: &str, token: &str) -> bool {
    let expected = format!("Bearer {token}");
    bool::from(subtle::ConstantTimeEq::ct_eq(
        received.as_bytes(),
        expected.as_bytes(),
    ))
}

fn get_command(path: &str) -> Option<Value> {
    match path {
        "/v1/ping" => Some(json!({"command":"ping"})),
        "/v1/methods" => Some(json!({"command":"methods"})),
        "/v1/server" => Some(json!({"command":"server_status"})),
        "/v1/capabilities" => Some(json!({"command":"debug_inspect","scope":"capabilities"})),
        "/v1/lifecycle" => Some(json!({"command":"debug_inspect","scope":"lifecycle"})),
        "/v1/timeline" => Some(json!({"command":"debug_inspect","scope":"timeline"})),
        "/v1/characters" => Some(json!({"command":"debug_inspect","scope":"character"})),
        "/v1/requests" => Some(json!({"command":"debug_inspect","scope":"requests"})),
        "/v1/presets" => Some(json!({"command":"debug_inspect","scope":"presets"})),
        "/v1/onboarding" => Some(json!({"command":"debug_inspect","scope":"onboarding"})),
        "/v1/gamelink" => Some(json!({"command":"game_link_inspect"})),
        "/v1/presenter/ai_runtime" => {
            Some(json!({"command":"presenter_inspect","surface":"ai_runtime"}))
        }
        "/v1/presenter/connections" => {
            Some(json!({"command":"presenter_inspect","surface":"connections"}))
        }
        "/v1/presenter/speech" => Some(json!({"command":"presenter_inspect","surface":"speech"})),
        "/v1/presenter/releases" => {
            Some(json!({"command":"presenter_inspect","surface":"releases"}))
        }
        _ => {
            if let Some(tail) = path.strip_prefix("/v1/gamelink/") {
                let (session, request) = tail.split_once("/request/")?;
                return Some(json!({"command":"game_link_inspect",
                    "session_id":Uuid::parse_str(session).ok()?,
                    "request_id":Uuid::parse_str(request).ok()?}));
            }
            if let Some(request) = path.strip_prefix("/v1/request/") {
                return Uuid::parse_str(request).ok().map(
                    |id| json!({"command":"debug_inspect","scope":"request","request_id":id}),
                );
            }
            if let Some(tail) = path.strip_prefix("/v1/character/")
                && let Some((character, event)) = tail.split_once("/event/")
            {
                if character.is_empty()
                    || character.len() > 48
                    || !character
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                {
                    return None;
                }
                return Uuid::parse_str(event).ok().map(|id| {
                    json!({"command":"debug_inspect","scope":"character", "character":character,"event_id":id})
                });
            }
            let (name, presenter) = match path.strip_prefix("/v1/presenter/chat/") {
                Some(name) => (name, true),
                None => (path.strip_prefix("/v1/character/")?, false),
            };
            if name.is_empty()
                || name.len() > 48
                || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return None;
            }
            if presenter {
                Some(json!({"command":"presenter_inspect","surface":"chat","character":name}))
            } else {
                Some(json!({"command":"debug_inspect","scope":"character","character":name}))
            }
        }
    }
}

fn admitted_post(value: Value, allow_write: bool) -> Result<Value, (u16, &'static str)> {
    let name = value
        .get("command")
        .and_then(Value::as_str)
        .ok_or((400, "missing command"))?;
    let read_only = matches!(
        name,
        "ping"
            | "methods"
            | "server_status"
            | "debug_inspect"
            | "presenter_inspect"
            | "game_link_inspect"
    );
    let mutating = matches!(
        name,
        "publish_message" | "reply" | "react" | "initiate" | "set_volume"
    );
    if !read_only && !mutating {
        return Err((403, "method is not exposed by HTTP gateway"));
    }
    if mutating && !allow_write {
        return Err((403, "writes disabled on HTTP gateway"));
    }
    if !value.is_object() {
        return Err((400, "expected JSON object"));
    }
    Ok(value)
}

fn body_error(message: &str) -> Value {
    json!({"type":"error","error":message})
}

async fn respond(stream: &mut TcpStream, status: u16, payload: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(payload).map_err(io::Error::other)?;
    let status_name = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "Internal Server Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {status_name}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&body).await
}

async fn handle(
    mut client: TcpStream,
    core: SocketAddr,
    token: Arc<str>,
    write_enabled: bool,
    port: u16,
) {
    let result = timeout(OPERATION_TIMEOUT, async {
        let request = read_request(&mut client).await.map_err(|_| (400, "invalid HTTP request"))?;
        if !matches!(request.host.as_str(), _ if request.host == format!("127.0.0.1:{port}") || request.host == format!("localhost:{port}")) {
            return Err((403, "untrusted Host header"));
        }
        if !authorized(&request.auth, &token) {
            return Err((401, "invalid credentials"));
        }
        let command = match request.method.as_str() {
            "GET" => get_command(&request.path).ok_or((404, "unknown endpoint"))?,
            "POST" if request.path == "/v1/commands" => {
                let value = serde_json::from_slice::<Value>(&request.body).map_err(|_| (400, "invalid JSON"))?;
                admitted_post(value, write_enabled)?
            }
            _ => return Err((404, "unknown endpoint")),
        };
        let response = call_local_control(core, OPERATION_TIMEOUT, command)
            .await
            .map_err(|_| (502, "Core unavailable or invalid response"))?;
        Ok::<Value, (u16, &str)>(response)
    }).await;
    let (status, response) = match result {
        Ok(Ok(value)) => (200, value),
        Ok(Err((code, message))) => (code, body_error(message)),
        Err(_) => (504, body_error("gateway operation timed out")),
    };
    let _ = respond(&mut client, status, &response).await;
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let options = config()?;
    let listener = TcpListener::bind(options.listen).await?;
    let port = listener.local_addr()?.port();
    let limit = Arc::new(Semaphore::new(16));
    loop {
        let (client, peer) = listener.accept().await?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let permit = limit
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let token = options.token.clone();
        let core = options.core;
        let writes = options.allow_write;
        tokio::spawn(async move {
            let _permit = permit;
            handle(client, core, token, writes, port).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_routes_and_write_guard() {
        assert!(get_command("/v1/presenter/chat/Crazy").is_some());
        assert_eq!(
            get_command("/v1/gamelink").unwrap()["command"],
            "game_link_inspect"
        );
        let scoped = format!("/v1/gamelink/{}/request/{}", Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            get_command(&scoped).unwrap()["command"],
            "game_link_inspect"
        );
        assert!(get_command("/v1/gamelink/../request/bad").is_none());
        assert!(get_command("/v1/timeline?dump=true").is_none());
        assert!(get_command("/v1/character/../../raw").is_none());
        assert!(admitted_post(json!({"command":"initiate"}), false).is_err());
        assert!(admitted_post(json!({"command":"cancel"}), true).is_err());
        assert!(admitted_post(json!({"command":"initiate"}), true).is_ok());
        assert!(admitted_post(json!({"command":"arbitrary"}), true).is_err());
    }

    #[test]
    fn bearer_requires_exact_secret() {
        assert!(authorized(
            "Bearer 12345678901234567890123456789012",
            "12345678901234567890123456789012"
        ));
        assert!(!authorized(
            "Bearer 12345678901234567890123456789011",
            "12345678901234567890123456789012"
        ));
        assert!(!authorized(
            "Basic 12345678901234567890123456789012",
            "12345678901234567890123456789012"
        ));
    }
}
