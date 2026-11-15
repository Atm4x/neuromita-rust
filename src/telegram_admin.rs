//! Application-owned Telegram MTProto session administration shared by GUI and MCP.
//! Credentials and authentication responses never enter observable snapshots.
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, watch};

use crate::{
    settings::{TtsBackend, TtsPreferences},
    telegram_sidecar::TelethonSession,
    telegram_voice::{
        DynamicTelegramVoice, FfmpegTelegramDecoder, TelegramConfig, TelegramError,
        TelegramVoiceService,
    },
};

#[derive(Clone, Debug, Serialize)]
pub struct TelegramAdminSnapshot {
    pub revision: u64,
    pub phase: u8,
    pub status: &'static str,
    pub authorized: bool,
    pub voice_active: bool,
}

struct SessionState {
    session: Option<Arc<TelethonSession>>,
}

pub struct TelegramAdmin {
    directory: PathBuf,
    router: Arc<DynamicTelegramVoice>,
    state: Mutex<SessionState>,
    status: watch::Sender<TelegramAdminSnapshot>,
}

impl TelegramAdmin {
    pub fn new(directory: PathBuf, router: Arc<DynamicTelegramVoice>) -> Self {
        let (status, _) = watch::channel(TelegramAdminSnapshot {
            revision: 0,
            phase: 0,
            status: "disconnected",
            authorized: false,
            voice_active: false,
        });
        Self {
            directory,
            router,
            state: Mutex::new(SessionState { session: None }),
            status,
        }
    }

    pub fn snapshot(&self) -> TelegramAdminSnapshot {
        self.status.borrow().clone()
    }

    pub fn observe(&self) -> watch::Receiver<TelegramAdminSnapshot> {
        self.status.subscribe()
    }

    fn update(
        &self,
        phase: u8,
        status: &'static str,
        authorized: bool,
        voice_active: bool,
    ) -> TelegramAdminSnapshot {
        self.status.send_modify(|snapshot| {
            snapshot.revision = snapshot.revision.saturating_add(1);
            snapshot.phase = phase;
            snapshot.status = status;
            snapshot.authorized = authorized;
            snapshot.voice_active = voice_active;
        });
        self.snapshot()
    }

    async fn open(
        &self,
        api_id: &str,
        api_hash: &str,
        phone: &str,
    ) -> Result<(Arc<TelethonSession>, bool), String> {
        let id = api_id
            .parse::<u32>()
            .map_err(|_| "invalid_api_id".to_owned())?;
        if api_hash.is_empty() || api_hash.len() > 128 || phone.is_empty() || phone.len() > 64 {
            return Err("invalid_credentials".into());
        }
        let python = std::env::var("NEUROMITA_PYTHON").unwrap_or_else(|_| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let sidecar = std::env::var_os("NEUROMITA_TELEGRAM_SIDECAR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| {
                        exe.parent()
                            .map(|parent| parent.join("tools/telegram_sidecar.py"))
                    })
                    .filter(|candidate| candidate.is_file())
                    .unwrap_or_else(|| {
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/telegram_sidecar.py")
                    })
            });
        if !sidecar.is_file() {
            return Err("sidecar_missing".into());
        }
        let session = TelethonSession::spawn(Path::new(&python), &sidecar)
            .map_err(|_| "sidecar_unavailable".to_owned())?;
        let authorized = session
            .connect(id, api_hash, phone, &self.directory.join("telegram-user"))
            .await
            .map_err(|error| error.to_string())?;
        if !authorized {
            session
                .request_code()
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok((session, authorized))
    }

    fn settings_enabled(prefs: &TtsPreferences) -> bool {
        prefs.enabled && prefs.backend == TtsBackend::Telegram
    }

    async fn apply_voice(
        &self,
        session: &Arc<TelethonSession>,
        prefs: &TtsPreferences,
    ) -> Result<bool, String> {
        if !Self::settings_enabled(prefs) {
            self.router.clear().await;
            return Ok(false);
        }
        let ffmpeg = std::env::var("NEUROMITA_FFMPEG").unwrap_or_else(|_| {
            if cfg!(windows) {
                "ffmpeg.exe".into()
            } else {
                "ffmpeg".into()
            }
        });
        let provider = TelegramVoiceService::new(
            session.clone() as Arc<dyn crate::telegram_voice::TelegramTransport>,
            Arc::new(FfmpegTelegramDecoder::new(ffmpeg)),
            TelegramConfig {
                peer: prefs.telegram_service.clone(),
                wait: Duration::from_secs(u64::from(prefs.telegram_wait_seconds)),
                interval: Duration::from_secs(u64::from(prefs.telegram_interval_seconds)),
                poll: Duration::from_millis(330),
                max_media_bytes: 32 * 1024 * 1024,
            },
        )
        .map_err(|error| error.to_string())?;
        self.router.install(Arc::new(provider)).await;
        Ok(true)
    }

    pub async fn connect(
        &self,
        api_id: &str,
        api_hash: &str,
        phone: &str,
        prefs: &TtsPreferences,
    ) -> Result<TelegramAdminSnapshot, String> {
        let mut guard = self.state.lock().await;
        self.router.clear().await;
        if let Some(old) = guard.session.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), old.disconnect()).await;
        }
        self.update(0, "connecting", false, false);
        match self.open(api_id, api_hash, phone).await {
            Ok((session, authorized)) => {
                let active = if authorized {
                    match self.apply_voice(&session, prefs).await {
                        Ok(active) => active,
                        Err(error) => {
                            guard.session = Some(session);
                            self.update(3, "voice_activation_failed", true, false);
                            return Err(error);
                        }
                    }
                } else {
                    false
                };
                guard.session = Some(session);
                Ok(self.update(
                    if authorized { 3 } else { 1 },
                    if authorized {
                        "connected"
                    } else {
                        "code_required"
                    },
                    authorized,
                    active,
                ))
            }
            Err(error) => {
                self.update(0, "connect_failed", false, false);
                Err(error)
            }
        }
    }

    pub async fn submit_code(
        &self,
        code: &str,
        prefs: &TtsPreferences,
    ) -> Result<TelegramAdminSnapshot, String> {
        if code.is_empty() || code.len() > 32 {
            return Err("invalid_code".into());
        }
        let guard = self.state.lock().await;
        let session = guard.session.as_ref().ok_or("no_session")?;
        match session.sign_in_code(code).await {
            Ok(true) => Ok(self.update(2, "password_required", false, false)),
            Ok(false) => {
                let active = self.apply_voice(session, prefs).await?;
                Ok(self.update(3, "connected", true, active))
            }
            Err(TelegramError::ExpiredCode) => {
                session
                    .request_code()
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(self.update(1, "code_expired_retry", false, false))
            }
            Err(error) => {
                self.update(1, "code_rejected", false, false);
                Err(error.to_string())
            }
        }
    }

    pub async fn submit_password(
        &self,
        password: &str,
        prefs: &TtsPreferences,
    ) -> Result<TelegramAdminSnapshot, String> {
        if password.is_empty() || password.len() > 1024 {
            return Err("invalid_password".into());
        }
        let guard = self.state.lock().await;
        let session = guard.session.as_ref().ok_or("no_session")?;
        match session.sign_in_password(password).await {
            Ok(()) => {
                let active = self.apply_voice(session, prefs).await?;
                Ok(self.update(3, "connected", true, active))
            }
            Err(error) => {
                self.update(2, "password_rejected", false, false);
                Err(error.to_string())
            }
        }
    }

    pub async fn refresh_voice(
        &self,
        prefs: &TtsPreferences,
    ) -> Result<TelegramAdminSnapshot, String> {
        let guard = self.state.lock().await;
        let previous = self.snapshot();
        let Some(session) = guard.session.as_ref() else {
            self.router.clear().await;
            return Ok(self.update(0, "disconnected", false, false));
        };
        if !previous.authorized {
            self.router.clear().await;
            return Ok(previous);
        }
        let active = self.apply_voice(session, prefs).await?;
        Ok(self.update(3, "connected", true, active))
    }

    pub async fn disconnect(&self) -> TelegramAdminSnapshot {
        let mut guard = self.state.lock().await;
        self.router.clear().await;
        if let Some(session) = guard.session.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), session.disconnect()).await;
        }
        self.update(0, "disconnected", false, false)
    }

    pub async fn auto_connect(
        &self,
        prefs: &TtsPreferences,
    ) -> Result<TelegramAdminSnapshot, String> {
        if !Self::settings_enabled(prefs) || !prefs.telegram_auto_connect {
            return Ok(self.snapshot());
        }
        let api_id = std::env::var("NEUROMITA_TG_API_ID").map_err(|_| "credentials_missing")?;
        let hash = std::env::var("NEUROMITA_TG_API_HASH").map_err(|_| "credentials_missing")?;
        let phone = std::env::var("NEUROMITA_TG_PHONE").map_err(|_| "credentials_missing")?;
        self.connect(&api_id, &hash, &phone, prefs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn status_is_sanitized_and_revisioned_without_an_authenticated_session() {
        let admin = TelegramAdmin::new(
            PathBuf::from("/tmp/neuromita-test"),
            Arc::new(DynamicTelegramVoice::default()),
        );
        assert_eq!(admin.snapshot().revision, 0);
        let previous = admin.observe();
        let result = admin.disconnect().await;
        assert_eq!(result.revision, 1);
        assert_eq!(result.status, "disconnected");
        assert!(!result.authorized);
        assert!(previous.has_changed().unwrap());
    }
}
