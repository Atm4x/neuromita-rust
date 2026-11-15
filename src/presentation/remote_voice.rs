//! Shared Remote Voice command/observation boundary; no GUI-held provider session.
use std::sync::Arc;
use tokio::sync::watch;

use crate::{
    character::CharacterId,
    remote_voice::{
        RemoteVoiceAdmin, RemoteVoiceAdminSnapshot, RemoteVoiceCatalogAction, RemoteVoiceConfig,
        RemoteVoiceError, RemoteVoicePreset, RemoteVoicePresetPatch,
    },
};

#[derive(Clone)]
pub struct RemoteVoicePresenter {
    owner: Arc<RemoteVoiceAdmin>,
}

impl RemoteVoicePresenter {
    pub(crate) fn new(owner: Arc<RemoteVoiceAdmin>) -> Self {
        Self { owner }
    }

    pub fn snapshot(&self) -> RemoteVoiceAdminSnapshot {
        self.owner.snapshot()
    }

    pub fn observe(&self) -> watch::Receiver<RemoteVoiceAdminSnapshot> {
        self.owner.observe()
    }

    pub fn config(&self) -> RemoteVoiceConfig {
        self.owner.config()
    }

    pub fn change(
        &self,
        action: RemoteVoiceCatalogAction,
        expected_revision: Option<u64>,
    ) -> Result<RemoteVoiceAdminSnapshot, RemoteVoiceError> {
        self.owner.change(action, expected_revision)
    }

    pub fn cancel(&self) -> RemoteVoiceAdminSnapshot {
        self.owner.cancel()
    }

    pub fn preview(
        &self,
        text: String,
        character: Option<CharacterId>,
        preset: RemoteVoicePreset,
        expected_revision: Option<u64>,
    ) -> Result<RemoteVoiceAdminSnapshot, RemoteVoiceError> {
        let player = self.player()?;
        self.owner
            .preview(text, character, preset, player, expected_revision)
    }

    pub fn preview_patch(
        &self,
        text: String,
        character: Option<CharacterId>,
        preset: RemoteVoicePresetPatch,
        expected_revision: u64,
    ) -> Result<RemoteVoiceAdminSnapshot, RemoteVoiceError> {
        let player = self.player()?;
        self.owner
            .preview_patch(text, character, preset, player, expected_revision)
    }

    fn player(&self) -> Result<std::path::PathBuf, RemoteVoiceError> {
        self.owner
            .ffplay_binary()
            .ok_or_else(|| RemoteVoiceError::InvalidPreset("FFplay executable unavailable".into()))
    }
}
