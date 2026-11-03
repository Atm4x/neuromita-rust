mod characters;
mod chat;
mod presets;
mod sandbox;
mod settings;

pub use characters::{
    CharactersPresenter, MemoryCapacityPreview, MemoryListPage, MemoryListQuery, MemoryObserver,
    MemorySummary,
};
pub(crate) use chat::PresenterRunner;
pub use chat::{ChatModel, ChatPresenter, PresenterError, RequestView};
pub use presets::PresetsPresenter;
pub use sandbox::{SandboxError, SandboxPresenter, SandboxSnapshot};
pub use settings::{
    BoundProperty, CharacterSettingsPresenter, CharactersSettingsPresenter,
    PresetsSettingsPresenter, PropertyObserver, SettingsPresenter, VoiceSettingsPresenter,
};

pub struct GuiPresenter {
    pub sandbox: SandboxPresenter,
    pub chat: ChatPresenter,
    pub settings: SettingsPresenter,
    pub presets: PresetsPresenter,
    pub characters: CharactersPresenter,
}
