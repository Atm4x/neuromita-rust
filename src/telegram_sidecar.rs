//! Explicit Telethon sidecar adapter. No Bot API and no implicit session creation.
//! The caller owns the Python executable, sidecar path and secure session location.
use crate::telegram_voice::{
    Incoming, MediaKind, TelegramError, TelegramFuture, TelegramTransport,
};
use base64::Engine;
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

struct Process {
    // Keeping the Child owned ensures kill_on_drop terminates the sidecar.
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}
pub struct TelethonSession {
    process: Mutex<Process>,
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
            process: Mutex::new(Process {
                _child: child,
                stdin,
                stdout: BufReader::new(stdout),
            }),
        }))
    }

    async fn call(&self, request: Value) -> Result<Value, TelegramError> {
        let encoded = serde_json::to_vec(&request).map_err(|_| TelegramError::Unavailable)?;
        let mut process = self.process.lock().await;
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
        let mut reply = String::new();
        let len = process
            .stdout
            .read_line(&mut reply)
            .await
            .map_err(|_| TelegramError::Disconnected)?;
        if len == 0 || len > 48 * 1024 * 1024 {
            return Err(TelegramError::Disconnected);
        }
        let response: Value = serde_json::from_str(&reply).map_err(|_| TelegramError::Network)?;
        if response["ok"] != true {
            return Err(error_code(
                response["error"].as_str().unwrap_or("telegram_failure"),
            ));
        }
        Ok(response["result"].clone())
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

    /// Returns true when Telegram requests an additional 2FA password.
    pub async fn sign_in_code(&self, code: &str) -> Result<bool, TelegramError> {
        let result = self.call(json!({"op":"sign_in","code":code})).await?;
        Ok(result["password_required"] == true)
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
}
