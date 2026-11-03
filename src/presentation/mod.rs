mod characters;
mod chat;
mod connections;
mod conversation;
mod presets;
mod releases;
mod settings;

pub use characters::{
    CharactersPresenter, MemoryCapacityPreview, MemoryListPage, MemoryListQuery, MemoryObserver,
    MemorySummary,
};
pub(crate) use chat::PresenterRunner;
pub use chat::{ChatModel, ChatPresenter, PresenterError, RequestView};
pub use connections::ConnectionsPresenter;
pub use conversation::{
    ConversationChange, ConversationChangePreview, ConversationError, ConversationPresenter,
    ConversationSnapshot,
};
pub use presets::PresetsPresenter;
pub use releases::{
    ReleaseCommandError, ReleaseLoadState, ReleasesLifecycle, ReleasesPresenter, ReleasesSnapshot,
};
pub use settings::{
    BoundProperty, CharacterSettingsPresenter, CharactersSettingsPresenter, PresetSlot,
    PresetsSettingsPresenter, PropertyObserver, SettingsPresenter, VoiceSettingsPresenter,
};

pub struct GuiPresenter {
    pub releases: ReleasesPresenter,
    pub connections: ConnectionsPresenter,
    pub conversation: ConversationPresenter,
    pub chat: ChatPresenter,
    pub settings: SettingsPresenter,
    pub presets: PresetsPresenter,
    pub characters: CharactersPresenter,
}
