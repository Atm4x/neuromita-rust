mod ai_engine;
mod characters;
mod chat;
mod connections;
mod conversation;
mod presets;
mod releases;
mod settings;

pub use ai_engine::{
    AiEngineSettingsPresenter, AiEngineSettingsSnapshot, AiHardwareDevice, AiHardwareSnapshot,
};
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
pub use presets::{
    PresetParameterOption, PresetView, PresetsApi, PresetsPresenter, PresetsView, ProviderOption,
};
pub use releases::{
    ReleaseCommandError, ReleaseLoadState, ReleasesLifecycle, ReleasesPresenter, ReleasesSnapshot,
};
pub use settings::{
    AsrSettingsPresenter, BoundProperty, CharacterSettingsPresenter, CharactersSettingsPresenter,
    InterfaceSettingsPresenter, PresetSlot, PresetsSettingsPresenter, PropertyObserver,
    SettingsPresenter, TtsSettingsPresenter, VoiceSettingsPresenter,
};

pub struct GuiPresenter {
    pub ai_engine: AiEngineSettingsPresenter,
    pub releases: ReleasesPresenter,
    pub connections: ConnectionsPresenter,
    pub conversation: ConversationPresenter,
    pub chat: ChatPresenter,
    pub settings: SettingsPresenter,
    pub presets: PresetsPresenter,
    pub characters: CharactersPresenter,
}
