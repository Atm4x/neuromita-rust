//! Single Telegram session API for all user-facing interfaces.
//! The Core service owns authorization and sidecar lifecycle; GUI/MCP receive
//! identical Presenter observations and call identical typed operations.
use std::sync::Arc;
use tokio::sync::watch;

use crate::telegram_admin::{TelegramAdmin, TelegramAdminSnapshot};

use super::SettingsPresenter;

#[derive(Clone)]
pub struct TelegramPresenter {
    owner: Arc<TelegramAdmin>,
    settings: Arc<SettingsPresenter>,
}

impl TelegramPresenter {
    pub(crate) fn new(owner: Arc<TelegramAdmin>, settings: Arc<SettingsPresenter>) -> Self {
        Self { owner, settings }
    }

    pub fn snapshot(&self) -> TelegramAdminSnapshot {
        self.owner.snapshot()
    }

    pub fn observe(&self) -> watch::Receiver<TelegramAdminSnapshot> {
        self.owner.observe()
    }

    pub async fn auto_connect(&self) -> Result<TelegramAdminSnapshot, String> {
        self.owner
            .auto_connect(&self.settings.tts.preferences.get())
            .await
    }

    pub async fn connect(
        &self,
        api_id: &str,
        api_hash: &str,
        phone: &str,
    ) -> Result<TelegramAdminSnapshot, String> {
        self.owner
            .connect(
                api_id,
                api_hash,
                phone,
                &self.settings.tts.preferences.get(),
            )
            .await
    }

    pub async fn submit_code(&self, code: &str) -> Result<TelegramAdminSnapshot, String> {
        self.owner
            .submit_code(code, &self.settings.tts.preferences.get())
            .await
    }

    pub async fn submit_password(&self, password: &str) -> Result<TelegramAdminSnapshot, String> {
        self.owner
            .submit_password(password, &self.settings.tts.preferences.get())
            .await
    }

    pub async fn refresh_voice(&self) -> Result<TelegramAdminSnapshot, String> {
        self.owner
            .refresh_voice(&self.settings.tts.preferences.get())
            .await
    }

    pub async fn disconnect(&self) -> TelegramAdminSnapshot {
        self.owner.disconnect().await
    }
}
