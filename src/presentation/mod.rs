mod characters;
mod chat;
mod presets;
mod settings;

pub use characters::CharactersPresenter;
pub(crate) use chat::PresenterRunner;
pub use chat::{ChatModel, ChatPresenter, PresenterError, RequestView};
pub use presets::PresetsPresenter;
pub use settings::{
    BoundProperty, CharacterSettingsPresenter, CharactersSettingsPresenter,
    PresetsSettingsPresenter, PropertyObserver, SettingsPresenter, VoiceSettingsPresenter,
};

pub struct GuiPresenter {
    pub chat: ChatPresenter,
    pub settings: SettingsPresenter,
    pub presets: PresetsPresenter,
    pub characters: CharactersPresenter,
}
