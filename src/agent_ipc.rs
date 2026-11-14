use std::{
    io::{self, Read},
    net::SocketAddr,
    path::Path,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpStream, tcp::OwnedReadHalf, tcp::OwnedWriteHalf},
    time::{Instant, timeout, timeout_at},
};
use uuid::Uuid;

const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_SCRIPT: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("usage: {0}")]
    Usage(String),
    #[error("unsafe command {0}: pass --allow-write to admit a Core mutation")]
    WriteDenied(String),
    #[error("invalid command: {0}")]
    Protocol(String),
    #[error("test assertion failed: expected subset {expected}, observed {observed}")]
    Assertion { expected: Value, observed: Value },
    #[error("the Core returned an error: {0}")]
    Core(String),
    #[error("IPC command timed out")]
    Timeout,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

struct Options {
    address: SocketAddr,
    timeout: Duration,
    allow_write: bool,
    include_private: bool,
    show_events: bool,
    await_terminal: bool,
    command: String,
    positional: Vec<String>,
    expect: Option<Value>,
    expect_terminal: Option<Value>,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, AgentError> {
        let mut address: SocketAddr = "127.0.0.1:8765".parse().expect("valid address");
        let mut timeout = Duration::from_secs(10);
        let mut allow_write = false;
        let mut include_private = false;
        let mut show_events = false;
        let mut await_terminal = false;
        let mut expect = None;
        let mut expect_terminal = None;
        let mut positional = Vec::new();
        let mut arguments = args.into_iter();
        while let Some(arg) = arguments.next() {
            match arg.as_str() {
                "--addr" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| AgentError::Usage("--addr requires an address".into()))?;
                    address = value
                        .parse()
                        .map_err(|_| AgentError::Usage("invalid --addr".into()))?;
                }
                "--timeout-ms" => {
                    let value = arguments.next().ok_or_else(|| {
                        AgentError::Usage("--timeout-ms requires an integer".into())
                    })?;
                    let value: u64 = value
                        .parse()
                        .map_err(|_| AgentError::Usage("invalid timeout".into()))?;
                    if !(1..=300_000).contains(&value) {
                        return Err(AgentError::Usage("timeout must be 1..300000 ms".into()));
                    }
                    timeout = Duration::from_millis(value);
                }
                "--expect-terminal" => {
                    let value = arguments.next().ok_or_else(|| {
                        AgentError::Usage("--expect-terminal requires JSON".into())
                    })?;
                    expect_terminal = Some(serde_json::from_str(&value)?);
                }
                "--allow-write" => allow_write = true,
                "--include-private" => include_private = true,
                "--events" => show_events = true,
                "--wait-terminal" => await_terminal = true,
                "--expect" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| AgentError::Usage("--expect requires JSON".into()))?;
                    expect = Some(serde_json::from_str(&value)?);
                }
                value if value.starts_with('-') && value != "-" => {
                    return Err(AgentError::Usage(format!("unknown option {value}")));
                }
                _ => positional.push(arg),
            }
        }
        if !address.ip().is_loopback() {
            return Err(AgentError::Usage(
                "IPC debug client is loopback-only".into(),
            ));
        }
        let command = positional.first().cloned().ok_or_else(|| AgentError::Usage(
            "neuromita-agent [--addr 127.0.0.1:PORT] inspect <scope> [character] | call '<json>' | batch <file|->".into()
        ))?;
        Ok(Self {
            address,
            timeout,
            allow_write,
            include_private,
            show_events,
            await_terminal,
            command,
            positional: positional.into_iter().skip(1).collect(),
            expect,
            expect_terminal,
        })
    }
}

fn is_read_only(method: &str) -> bool {
    matches!(
        method,
        "ping"
            | "methods"
            | "server_status"
            | "debug_inspect"
            | "presenter_inspect"
            | "diagnostics"
            | "diagnostics_recent"
            | "diagnostics_errors"
            | "settings"
    )
}

fn redacted(mut value: Value, include_private: bool) -> Value {
    if !include_private {
        redact(&mut value);
    }
    value
}

fn redact(value: &mut Value) {
    match value {
        Value::Array(rows) => rows.iter_mut().for_each(redact),
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if matches!(
                    key.as_str(),
                    "settings"
                        | "content"
                        | "text"
                        | "preview_text"
                        | "partial_text"
                        | "audio"
                        | "bytes"
                        | "api_key"
                        | "password"
                        | "authorization"
                        | "token"
                        | "error"
                        | "instruction"
                ) {
                    *value = Value::String("[redacted]".into());
                } else {
                    redact(value);
                }
            }
        }
        _ => {}
    }
}

fn is_subset(expected: &Value, observed: &Value) -> bool {
    match (expected, observed) {
        (Value::Object(left), Value::Object(right)) => left
            .iter()
            .all(|(key, v)| right.get(key).is_some_and(|other| is_subset(v, other))),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| is_subset(a, b))
        }
        _ => expected == observed,
    }
}

fn emit(value: &Value, include_private: bool) -> Result<(), AgentError> {
    println!(
        "{}",
        serde_json::to_string(&redacted(value.clone(), include_private))?
    );
    Ok(())
}

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    timeout: Duration,
}

impl Client {
    async fn connect(address: SocketAddr, timeout_limit: Duration) -> Result<Self, AgentError> {
        let stream = timeout(timeout_limit, TcpStream::connect(address))
            .await
            .map_err(|_| AgentError::Timeout)??;
        let (reader, writer) = stream.into_split();
        let mut client = Self {
            reader: BufReader::new(reader),
            writer,
            timeout: timeout_limit,
        };
        let hello = client.read_frame().await?;
        if hello["type"] != "connected" || !hello["client_id"].is_string() {
            return Err(AgentError::Protocol(
                "missing LocalControl handshake".into(),
            ));
        }
        Ok(client)
    }

    async fn read_frame(&mut self) -> Result<Value, AgentError> {
        let mut bytes = Vec::new();
        timeout(self.timeout, async {
            loop {
                let available = self.reader.fill_buf().await?;
                if available.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Core closed IPC",
                    ));
                }
                let newline = available.iter().position(|b| *b == b'\n');
                let count = newline.map_or(available.len(), |index| index + 1);
                if bytes.len().saturating_add(count) > MAX_FRAME {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "IPC response exceeds limit",
                    ));
                }
                bytes.extend_from_slice(&available[..count]);
                self.reader.consume(count);
                if newline.is_some() {
                    break;
                }
            }
            Ok::<_, io::Error>(())
        })
        .await
        .map_err(|_| AgentError::Timeout)??;
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn send(&mut self, command: &Value) -> Result<(), AgentError> {
        let bytes = serde_json::to_vec(command)?;
        if bytes.len() > 64 * 1024 - 1 {
            return Err(AgentError::Protocol(
                "IPC command exceeds maximum frame".into(),
            ));
        }
        timeout(self.timeout, async {
            self.writer.write_all(&bytes).await?;
            self.writer.write_all(b"\n").await
        })
        .await
        .map_err(|_| AgentError::Timeout)??;
        Ok(())
    }

    async fn read_until(
        &mut self,
        deadline: Instant,
        opts: &Options,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value, AgentError> {
        timeout_at(deadline, async {
            loop {
                let frame = self.read_frame().await?;
                if predicate(&frame) {
                    return Ok(frame);
                }
                if opts.show_events && frame["type"] != "settings" {
                    emit(&frame, opts.include_private)?;
                }
            }
        })
        .await
        .map_err(|_| AgentError::Timeout)?
    }

    async fn response(
        &mut self,
        id: Uuid,
        deadline: Instant,
        opts: &Options,
    ) -> Result<Value, AgentError> {
        let expected_id = id.to_string();
        self.read_until(deadline, opts, |frame| frame["command_id"] == expected_id)
            .await
    }

    async fn terminal(
        &mut self,
        request_id: &Value,
        deadline: Instant,
        opts: &Options,
    ) -> Result<Value, AgentError> {
        self.read_until(deadline, opts, |frame| {
            frame["type"] == "request_status"
                && frame["request_id"] == *request_id
                && matches!(frame["state"].as_str(), Some("finished" | "cancelled"))
        })
        .await
    }

    async fn invoke(&mut self, mut command: Value, opts: &Options) -> Result<(), AgentError> {
        let object = command
            .as_object_mut()
            .ok_or_else(|| AgentError::Protocol("command must be a JSON object".into()))?;
        let expect = object.remove("expect").or_else(|| opts.expect.clone());
        let expect_terminal = object
            .remove("expect_terminal")
            .or_else(|| opts.expect_terminal.clone());
        let wait_terminal = match object.remove("wait_terminal") {
            Some(Value::Bool(value)) => value,
            Some(_) => {
                return Err(AgentError::Protocol(
                    "wait_terminal must be a boolean".into(),
                ));
            }
            None => opts.await_terminal,
        } || expect_terminal.is_some();
        let name = object
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::Protocol("missing command string".into()))?
            .to_owned();
        if !is_read_only(&name) && !opts.allow_write {
            return Err(AgentError::WriteDenied(name));
        }
        let id = match object.get("command_id") {
            Some(Value::String(id)) => Uuid::parse_str(id)
                .map_err(|_| AgentError::Protocol("invalid command_id".into()))?,
            Some(_) => {
                return Err(AgentError::Protocol(
                    "command_id must be UUID string".into(),
                ));
            }
            None => {
                let id = Uuid::new_v4();
                object.insert("command_id".into(), json!(id));
                id
            }
        };
        let deadline = Instant::now() + self.timeout;
        timeout_at(deadline, self.send(&command))
            .await
            .map_err(|_| AgentError::Timeout)??;
        let response = self.response(id, deadline, opts).await?;
        if let Some(expected) = expect {
            if !is_subset(&expected, &response) {
                return Err(AgentError::Assertion {
                    expected,
                    observed: redacted(response, opts.include_private),
                });
            }
        } else if response["type"] == "error" {
            return Err(AgentError::Core(
                response["code"].as_str().unwrap_or("unknown").into(),
            ));
        }
        emit(&response, opts.include_private)?;
        if wait_terminal {
            if response["type"] != "accepted" || !response["request_id"].is_string() {
                return Err(AgentError::Protocol(
                    "terminal wait requires an accepted request with request_id".into(),
                ));
            }
            let terminal = self
                .terminal(&response["request_id"], deadline, opts)
                .await?;
            emit(&terminal, opts.include_private)?;
            if let Some(expected) = expect_terminal {
                if !is_subset(&expected, &terminal) {
                    return Err(AgentError::Assertion {
                        expected,
                        observed: redacted(terminal, opts.include_private),
                    });
                }
            } else if terminal["state"] != "finished" || terminal["outcome"] != "success" {
                return Err(AgentError::Core(format!(
                    "request terminal state={}, outcome={}",
                    terminal["state"].as_str().unwrap_or("unknown"),
                    terminal["outcome"].as_str().unwrap_or("unknown"),
                )));
            }
        }
        Ok(())
    }
}

pub async fn run(args: impl IntoIterator<Item = String>) -> Result<(), AgentError> {
    let opts = Options::parse(args)?;
    let commands = match opts.command.as_str() {
        "inspect" => {
            if !(1..=2).contains(&opts.positional.len()) {
                return Err(AgentError::Usage(
                    "inspect <capabilities|lifecycle|timeline|character|requests> [character]"
                        .into(),
                ));
            }
            let mut command = json!({"command":"debug_inspect", "scope":opts.positional[0]});
            if let Some(character) = opts.positional.get(1) {
                command["character"] = json!(character);
            }
            vec![command]
        }
        "presenter" => {
            if !(1..=2).contains(&opts.positional.len()) {
                return Err(AgentError::Usage(
                    "presenter <chat|ai_runtime|connections> [character]".into(),
                ));
            }
            let mut command = json!({"command":"presenter_inspect", "surface":opts.positional[0]});
            if let Some(character) = opts.positional.get(1) {
                command["character"] = json!(character);
            }
            vec![command]
        }
        "call" => {
            if opts.positional.len() != 1 {
                return Err(AgentError::Usage("call '<JSON command>'".into()));
            }
            vec![serde_json::from_str(&opts.positional[0])?]
        }
        "batch" => {
            if opts.positional.len() != 1 {
                return Err(AgentError::Usage("batch <script.jsonl|->".into()));
            }
            let content = if opts.positional[0] == "-" {
                let mut bytes = Vec::new();
                io::stdin()
                    .take((MAX_SCRIPT + 1) as u64)
                    .read_to_end(&mut bytes)?;
                String::from_utf8(bytes)
                    .map_err(|_| AgentError::Protocol("script must be UTF-8".into()))?
            } else {
                let path = Path::new(&opts.positional[0]);
                let meta = std::fs::metadata(path)?;
                if meta.len() > MAX_SCRIPT as u64 {
                    return Err(AgentError::Protocol("script too large".into()));
                }
                std::fs::read_to_string(path)?
            };
            if content.len() > MAX_SCRIPT {
                return Err(AgentError::Protocol("script too large".into()));
            }
            let commands = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str::<Value>)
                .collect::<Result<Vec<_>, _>>()?;
            if commands.is_empty() || commands.len() > 128 {
                return Err(AgentError::Protocol(
                    "script requires 1..128 commands".into(),
                ));
            }
            commands
        }
        other => return Err(AgentError::Usage(format!("unknown operation {other}"))),
    };
    for value in &commands {
        let name = value
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::Protocol("every command needs a method".into()))?;
        if !is_read_only(name) && !opts.allow_write {
            return Err(AgentError::WriteDenied(name.into()));
        }
    }
    let mut client = Client::connect(opts.address, opts.timeout).await?;
    for command in commands {
        client.invoke(command, &opts).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_mutation_gate_and_loopback_only() {
        assert!(is_read_only("debug_inspect"));
        assert!(is_read_only("presenter_inspect"));
        assert!(!is_read_only("initiate"));
        assert!(!is_read_only("arbitrary_unknown_method"));
        assert!(matches!(
            Options::parse([
                "--addr".into(),
                "10.1.2.3:4321".into(),
                "inspect".into(),
                "timeline".into()
            ]),
            Err(AgentError::Usage(_))
        ));
    }

    #[test]
    fn recursive_redaction_keeps_structural_metadata() {
        let mut value = json!({"type":"request_status", "request_id":"uuid", "settings":{"voice":{"api_key":"secret"}}, "events":[{"text":"private", "outcome":"success"}]});
        redact(&mut value);
        assert_eq!(value["type"], "request_status");
        assert_eq!(value["events"][0]["outcome"], "success");
        assert_eq!(value["settings"], "[redacted]");
        assert_eq!(value["events"][0]["text"], "[redacted]");
    }

    #[test]
    fn assertions_compare_nested_subset_without_ignoring_wrong_values() {
        let actual = json!({"type":"debug_inspect","snapshot":{"head_sequence":3,"selection_matches_head":true}});
        assert!(is_subset(&json!({"snapshot":{"head_sequence":3}}), &actual));
        assert!(!is_subset(
            &json!({"snapshot":{"head_sequence":4}}),
            &actual
        ));
    }

    #[tokio::test]
    async fn unrelated_frames_cannot_extend_the_command_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            peer.write_all(b"{\"type\":\"connected\",\"client_id\":\"fixture\"}\n")
                .await
                .unwrap();
            for _ in 0..120 {
                if peer
                    .write_all(b"{\"type\":\"request_status\",\"request_id\":\"foreign\"}\n")
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        });
        let opts = Options::parse([
            "--addr".into(),
            address.to_string(),
            "--timeout-ms".into(),
            "60".into(),
            "call".into(),
            r#"{"command":"ping"}"#.into(),
        ])
        .unwrap();
        let mut client = Client::connect(opts.address, opts.timeout).await.unwrap();
        let deadline = Instant::now() + Duration::from_millis(60);
        let start = Instant::now();
        let result = client.response(Uuid::new_v4(), deadline, &opts).await;
        assert!(matches!(result, Err(AgentError::Timeout)));
        assert!(start.elapsed() < Duration::from_millis(250));
        server.abort();
    }

    #[tokio::test]
    async fn terminal_error_fails_unless_explicitly_expected() {
        async fn check(expect_failure: bool) -> Result<(), AgentError> {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut peer, _) = listener.accept().await.unwrap();
                peer.write_all(b"{\"type\":\"connected\",\"client_id\":\"fixture\"}\n")
                    .await
                    .unwrap();
                let mut reader = BufReader::new(peer);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let command: Value = serde_json::from_str(&line).unwrap();
                let id = command["command_id"].clone();
                let mut peer = reader.into_inner();
                let accepted =
                    json!({"type":"accepted","command_id":id,"request_id":Uuid::new_v4()});
                peer.write_all(&serde_json::to_vec(&accepted).unwrap())
                    .await
                    .unwrap();
                peer.write_all(b"\n").await.unwrap();
                let terminal = json!({"type":"request_status","request_id":accepted["request_id"],"state":"finished","outcome":"text_error","text":"not allowed"});
                peer.write_all(&serde_json::to_vec(&terminal).unwrap())
                    .await
                    .unwrap();
                peer.write_all(b"\n").await.unwrap();
            });
            let mut args = vec![
                "--addr".to_owned(),
                address.to_string(),
                "--timeout-ms".to_owned(),
                "2000".to_owned(),
                "--allow-write".to_owned(),
                "--wait-terminal".to_owned(),
            ];
            if expect_failure {
                args.extend([
                    "--expect-terminal".to_owned(),
                    r#"{"state":"finished","outcome":"text_error"}"#.to_owned(),
                ]);
            }
            args.extend([
                "call".to_owned(),
                r#"{"command":"initiate","character":"Crazy"}"#.to_owned(),
            ]);
            let result = run(args).await;
            server.await.unwrap();
            result
        }
        assert!(matches!(check(false).await, Err(AgentError::Core(_))));
        check(true).await.unwrap();
    }
}
