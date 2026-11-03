use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio::sync::watch;

use super::{BoundProperty, CharactersPresenter, ChatModel, ChatPresenter, PresenterError};
use crate::{
    character::{CharacterError, CharacterId, CharacterState},
    generation::{
        PresetCatalog, PresetCatalogError, PresetCatalogSnapshot, PresetId, ProviderTemplate,
    },
    request::{CancelResult, RequestHandle, RequestId},
    settings::{PresetSelection, SettingsError, SettingsOwner, SettingsSnapshot, Volume},
    timeline::{
        ContextAnchor, SessionTimeline, domain::ProjectedMessage, selection::TimelineSelection,
    },
};

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error(transparent)]
    Character(#[from] CharacterError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Preset(#[from] PresetCatalogError),
    #[error("timeline is unavailable")]
    TimelineUnavailable,
}

pub struct SandboxSnapshot {
    pub character: CharacterId,
    pub at: ContextAnchor,
    pub state: CharacterState,
    pub messages: Vec<ProjectedMessage>,
    pub requests: Arc<ChatModel>,
    pub settings: Arc<SettingsSnapshot>,
    pub preset: PresetId,
    pub model: String,
}

#[derive(Clone)]
pub struct SandboxPresenter {
    chat: ChatPresenter,
    characters: CharactersPresenter,
    timeline: Arc<Mutex<SessionTimeline>>,
    settings: Arc<SettingsOwner>,
    presets: Arc<PresetCatalog>,
    volume: BoundProperty<Volume>,
}

impl SandboxPresenter {
    pub(crate) fn new(
        chat: ChatPresenter,
        characters: CharactersPresenter,
        timeline: Arc<Mutex<SessionTimeline>>,
        settings: Arc<SettingsOwner>,
        presets: Arc<PresetCatalog>,
    ) -> Self {
        let volume = super::SettingsPresenter::new(Arc::clone(&settings))
            .voice
            .volume;
        Self {
            chat,
            characters,
            timeline,
            settings,
            presets,
            volume,
        }
    }

    pub fn characters(&self) -> &'static [CharacterId] {
        self.characters.available()
    }

    pub fn reply(
        &self,
        character: CharacterId,
        text: String,
    ) -> Result<RequestHandle, PresenterError> {
        self.chat.reply(character, text)
    }

    pub fn cancel(&self, request: RequestId) -> Result<CancelResult, PresenterError> {
        self.chat.cancel(request)
    }

    pub fn set_volume(&self, value: Volume) -> Result<(), SettingsError> {
        self.volume.set(value)
    }

    pub fn observe_requests(&self) -> watch::Receiver<Arc<ChatModel>> {
        self.chat.observe()
    }

    pub fn observe_settings(
        &self,
    ) -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        self.settings.observe()
    }

    pub fn observe_presets(&self) -> watch::Receiver<Arc<PresetCatalogSnapshot>> {
        self.presets.observe()
    }

    pub fn observe_selection(&self) -> Result<watch::Receiver<TimelineSelection>, SandboxError> {
        Ok(self
            .timeline
            .lock()
            .map_err(|_| SandboxError::TimelineUnavailable)?
            .observe_selection())
    }

    /// Returns a fixed-prefix character view and independent request observations.
    /// A terminal request alone does not prove membership in the selected branch.
    pub async fn snapshot(&self, character: CharacterId) -> Result<SandboxSnapshot, SandboxError> {
        let requests = self.chat.snapshot();
        let at = self
            .timeline
            .lock()
            .map_err(|_| SandboxError::TimelineUnavailable)?
            .anchor();
        let view = self.characters.context(character, at).await?;
        let settings = self.settings.snapshot_all()?;
        let assigned = settings
            .values
            .characters
            .for_character(character)
            .presets
            .reply;
        let preset = match assigned {
            PresetSelection::Active => settings.values.presets.active,
            PresetSelection::Specific(id) => id,
        };
        let resolved = self
            .presets
            .get(preset)?
            .ok_or(PresetCatalogError::NotFound(preset))?;
        let model = match resolved.template() {
            ProviderTemplate::Gemini(template) => template.model().to_owned(),
        };
        Ok(SandboxSnapshot {
            character,
            at,
            state: view.state,
            messages: view.view.messages(),
            requests,
            settings,
            preset,
            model,
        })
    }
}
