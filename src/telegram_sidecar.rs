//! Explicit Telethon sidecar adapter. No Bot API and no implicit session creation.
//! The caller owns the Python executable, sidecar path and secure session location.
use crate::telegram_voice::{
    Incoming, MediaKind, TelegramError, TelegramFuture, TelegramTransport,
};
use base64::Engine;
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

struct Process {
    // Keeping the Child owned ensures kill_on_drop terminates the sidecar.
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_request_id: u64,
}
pub struct TelethonSession {
    process: Arc<Mutex<Process>>,
}

fn error_code(text: &str) -> TelegramError {
    match text {
        "invalid_code" => TelegramError::InvalidCode,
        "expired_code" => TelegramError::ExpiredCode,
        "invalid_password" => TelegramError::InvalidPassword,
        "unauthorized" | "code_not_requested" => TelegramError::Unauthorized,
        "not_connected" => TelegramError::Disconnected,
        "media_too_large" => TelegramError::MediaTooLarge,
        "missing_media" => TelegramError::InvalidMedia,
        "telethon_not_installed" => TelegramError::Unavailable,
        _ => TelegramError::Network,
    }
}

/// Treat an inconclusive sidecar auth reply as unauthorized, never as success.
fn code_sign_in_result(result: &Value) -> Result<bool, TelegramError> {
    if result["password_required"] == true {
        Ok(true)
    } else if result["authorized"] == true {
        Ok(false)
    } else {
        Err(TelegramError::Unauthorized)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ResponseId {
    Stale,
    Matched,
}

fn check_response_id(response: &Value, expected: u64) -> Result<ResponseId, TelegramError> {
    match response["request_id"].as_u64() {
        Some(actual) if actual < expected => Ok(ResponseId::Stale),
        Some(actual) if actual == expected => Ok(ResponseId::Matched),
        _ => Err(TelegramError::Network),
    }
}

/// Read one newline-delimited response without unbounded allocation. A
/// base64-encoded 32 MiB media payload fits below this response limit.
async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    max: usize,
) -> Result<Option<Vec<u8>>, TelegramError> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| TelegramError::Disconnected)?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(TelegramError::Disconnected)
            };
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if count > max.saturating_sub(line.len()) {
            return Err(TelegramError::MediaTooLarge);
        }
        let complete = available[count - 1] == b'\n';
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if complete {
            return Ok(Some(line));
        }
    }
}

impl TelethonSession {
    /// Start a long-lived user-session transport. Requires installed Telethon.
    pub fn spawn(python: &Path, sidecar: &Path) -> Result<Arc<Self>, TelegramError> {
        let mut child = Command::new(python)
            .arg("-u")
            .arg(sidecar)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| TelegramError::Unavailable)?;
        let stdin = child.stdin.take().ok_or(TelegramError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(TelegramError::Unavailable)?;
        Ok(Arc::new(Self {
            process: Arc::new(Mutex::new(Process {
                _child: child,
                stdin,
                stdout: BufReader::new(stdout),
                next_request_id: 0,
            })),
        }))
    }

    async fn call(&self, request: Value) -> Result<Value, TelegramError> {
        // An RPC must finish draining its sidecar frame even when the caller's
        // generation task is dropped. Otherwise the next RPC could see the
        // tail of a partially consumed response. The owned worker is bounded.
        let process = Arc::clone(&self.process);
        tokio::spawn(async move { Self::call_held(process, request).await })
            .await
            .map_err(|_| TelegramError::Disconnected)?
    }

    async fn call_held(
        process: Arc<Mutex<Process>>,
        mut request: Value,
    ) -> Result<Value, TelegramError> {
        let mut process = process.lock().await;
        // An abandoned RPC can still return after the caller has been cancelled.
        // Correlate every sidecar response to its request, never borrow a stale one.
        process.next_request_id = process
            .next_request_id
            .checked_add(1)
            .ok_or(TelegramError::Unavailable)?;
        let request_id = process.next_request_id;
        request
            .as_object_mut()
            .ok_or(TelegramError::Unavailable)?
            .insert("request_id".into(), json!(request_id));
        let encoded = serde_json::to_vec(&request).map_err(|_| TelegramError::Unavailable)?;
        if encoded.len() > 16 * 1024 {
            return Err(TelegramError::Unavailable);
        }

        let result = tokio::time::timeout(Duration::from_secs(60), async {
            process
                .stdin
                .write_all(&encoded)
                .await
                .map_err(|_| TelegramError::Disconnected)?;
            process
                .stdin
                .write_all(b"\n")
                .await
                .map_err(|_| TelegramError::Disconnected)?;
            process
                .stdin
                .flush()
                .await
                .map_err(|_| TelegramError::Disconnected)?;
            loop {
                let line = match read_bounded_line(&mut process.stdout, 48 * 1024 * 1024).await {
                    Ok(Some(line)) => line,
                    Ok(None) => return Err(TelegramError::Disconnected),
                    Err(TelegramError::MediaTooLarge) => {
                        let _ = process._child.start_kill();
                        return Err(TelegramError::MediaTooLarge);
                    }
                    Err(error) => return Err(error),
                };
                let response: Value =
                    serde_json::from_slice(&line).map_err(|_| TelegramError::Network)?;
                if response["request_id"].as_u64().is_none()
                    && response["error"] == "telethon_not_installed"
                {
                    return Err(TelegramError::Unavailable);
                }
                match check_response_id(&response, request_id)? {
                    ResponseId::Stale => continue,
                    ResponseId::Matched => {}
                }
                if response["ok"] != true {
                    return Err(error_code(
                        response["error"].as_str().unwrap_or("telegram_failure"),
                    ));
                }
                return Ok(response["result"].clone());
            }
        })
        .await;
        match result {
            Ok(value) => value,
            Err(_) => {
                // If a read timed out mid-frame, the newline stream cannot be
                // safely reused. Terminate this session rather than mix replies.
                let _ = process._child.start_kill();
                Err(TelegramError::Timeout)
            }
        }
    }

    /// A persistent Telethon SQLite session is a credential, not ordinary settings.
    /// The embedding application must choose a protected path outside the repository.
    pub async fn connect(
        &self,
        api_id: u32,
        api_hash: &str,
        phone: &str,
        session_path: &Path,
    ) -> Result<bool, TelegramError> {
        if api_id == 0 || api_hash.is_empty() || phone.is_empty() {
            return Err(TelegramError::Unavailable);
        }
        let session = session_path.to_str().ok_or(TelegramError::Unavailable)?;
        let result = self
            .call(json!({
                "op": "connect", "api_id": api_id,
                "api_hash": api_hash, "phone": phone, "session": session
            }))
            .await?;
        Ok(result["authorized"] == true)
    }

    pub async fn request_code(&self) -> Result<(), TelegramError> {
        self.call(json!({"op":"request_code"})).await?;
        Ok(())
    }

    /// Returns true only when Telegram requests an additional 2FA password.
    /// A reply that confirms neither 2FA nor authorization must fail closed.
    pub async fn sign_in_code(&self, code: &str) -> Result<bool, TelegramError> {
        let result = self.call(json!({"op":"sign_in","code":code})).await?;
        code_sign_in_result(&result)
    }

    pub async fn sign_in_password(&self, password: &str) -> Result<(), TelegramError> {
        let result = self
            .call(json!({"op":"password","password":password}))
            .await?;
        if result["authorized"] != true {
            return Err(TelegramError::Unauthorized);
        }
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<(), TelegramError> {
        self.call(json!({"op":"disconnect"})).await?;
        Ok(())
    }
}

impl TelegramTransport for TelethonSession {
    fn ready(&self) -> TelegramFuture<'_, ()> {
        Box::pin(async move {
            let result = self.call(json!({"op":"status"})).await?;
            if result["authorized"] == true && result["connected"] == true {
                Ok(())
            } else {
                Err(TelegramError::Unauthorized)
            }
        })
    }

    fn latest<'a>(&'a self, peer: &'a str) -> TelegramFuture<'a, i64> {
        Box::pin(async move {
            let result = self.call(json!({"op":"latest","peer":peer})).await?;
            result["id"].as_i64().ok_or(TelegramError::Network)
        })
    }

    fn send<'a>(&'a self, peer: &'a str, text: &'a str) -> TelegramFuture<'a, i64> {
        Box::pin(async move {
            let result = self
                .call(json!({"op":"send","peer":peer,"text":text}))
                .await?;
            result["id"].as_i64().ok_or(TelegramError::Network)
        })
    }

    fn recent<'a>(&'a self, peer: &'a str, after: i64) -> TelegramFuture<'a, Vec<Incoming>> {
        Box::pin(async move {
            let result = self
                .call(json!({"op":"recent","peer":peer,"after":after}))
                .await?;
            let rows = result["messages"]
                .as_array()
                .ok_or(TelegramError::Network)?;
            rows.iter()
                .map(|row| {
                    let id = row["id"].as_i64().ok_or(TelegramError::Network)?;
                    let actual_peer = row["peer"].as_str().ok_or(TelegramError::Network)?;
                    let outgoing = row["outgoing"].as_bool().ok_or(TelegramError::Network)?;
                    let kind = match row["kind"].as_str() {
                        Some("mp3") => Some(MediaKind::Mp3),
                        Some("voice_ogg") => Some(MediaKind::VoiceOgg),
                        Some(_) => Some(MediaKind::Other),
                        None => None,
                    };
                    Ok(Incoming {
                        id,
                        peer: actual_peer.into(),
                        outgoing,
                        kind,
                        reply_to_id: row["reply_to_id"].as_i64(),
                    })
                })
                .collect()
        })
    }

    fn download<'a>(&'a self, peer: &'a str, id: i64, limit: usize) -> TelegramFuture<'a, Vec<u8>> {
        Box::pin(async move {
            let result = self
                .call(json!({"op":"download","peer":peer,"id":id,"limit":limit}))
                .await?;
            let encoded = result["data"].as_str().ok_or(TelegramError::InvalidMedia)?;
            // Bound the returned allocation before base64 decoding.
            if encoded.len() > limit.saturating_mul(4).saturating_div(3).saturating_add(16) {
                return Err(TelegramError::MediaTooLarge);
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| TelegramError::InvalidMedia)?;
            if bytes.len() > limit {
                return Err(TelegramError::MediaTooLarge);
            }
            Ok(bytes)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auth_errors_are_classified_without_disclosing_secrets() {
        assert_eq!(error_code("invalid_code"), TelegramError::InvalidCode);
        assert_eq!(error_code("expired_code"), TelegramError::ExpiredCode);
        assert_eq!(error_code("media_too_large"), TelegramError::MediaTooLarge);
        assert_eq!(error_code("telegram_failure"), TelegramError::Network);
    }

    #[test]
    fn sign_in_requires_explicit_authorization_or_2fa() {
        assert_eq!(code_sign_in_result(&json!({"authorized": true})), Ok(false));
        assert_eq!(
            code_sign_in_result(&json!({"password_required": true})),
            Ok(true)
        );
        assert_eq!(
            code_sign_in_result(&json!({"authorized": false})),
            Err(TelegramError::Unauthorized)
        );
        assert_eq!(
            code_sign_in_result(&json!({})),
            Err(TelegramError::Unauthorized)
        );
    }

    #[test]
    fn cancelled_rpc_response_cannot_be_mistaken_for_next_request() {
        assert_eq!(
            check_response_id(&json!({"request_id": 41}), 42).unwrap(),
            ResponseId::Stale
        );
        assert_eq!(
            check_response_id(&json!({"request_id": 42}), 42).unwrap(),
            ResponseId::Matched
        );
        assert_eq!(
            check_response_id(&json!({"request_id": 43}), 42),
            Err(TelegramError::Network)
        );
        assert_eq!(
            check_response_id(&json!({"result": {}}), 42),
            Err(TelegramError::Network)
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn aborted_caller_does_not_corrupt_the_next_sidecar_frame() {
        let script = std::env::temp_dir().join(format!(
            "neuromita-fake-telegram-{}.py",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &script,
            r#"import json, sys, time
for line in sys.stdin:
    req = json.loads(line)
    if req["request_id"] == 1:
        time.sleep(0.2)
    print(json.dumps({"request_id": req["request_id"], "ok": True,
                      "result": {"authorized": True, "connected": True}}), flush=True)
"#,
        )
        .unwrap();
        let session = TelethonSession::spawn(Path::new("python3"), &script).unwrap();
        let first = Arc::clone(&session);
        let task = tokio::spawn(async move { first.ready().await });
        tokio::time::sleep(Duration::from_millis(60)).await;
        task.abort();
        let second = tokio::time::timeout(Duration::from_secs(3), session.ready())
            .await
            .unwrap();
        assert_eq!(second, Ok(()));
        drop(session);
        let _ = std::fs::remove_file(script);
    }

    #[tokio::test]
    async fn sidecar_line_has_hard_limit_and_requires_newline() {
        let mut valid = BufReader::new(&b"{\"ok\":true}\n"[..]);
        assert_eq!(
            read_bounded_line(&mut valid, 32).await.unwrap(),
            Some(b"{\"ok\":true}\n".to_vec())
        );
        let mut oversized = BufReader::new(&b"0123456789\n"[..]);
        assert_eq!(
            read_bounded_line(&mut oversized, 8).await,
            Err(TelegramError::MediaTooLarge)
        );
        let mut truncated = BufReader::new(&b"partial"[..]);
        assert_eq!(
            read_bounded_line(&mut truncated, 32).await,
            Err(TelegramError::Disconnected)
        );
    }
}
