pub mod ai_hub_catalog;
pub mod ai_install_operation;
pub mod native_download;
pub mod native_hub;
pub mod native_model;
pub mod native_release_catalog;
pub mod native_revisions;
pub use ai_install_operation::AiInstallOperationPresenter;
mod audio_input;
mod characters;
pub use audio_input::AudioInputPresenter;
mod chat;
mod connections;
mod conversation;
mod guide_persistence;
pub mod hardware;
mod ipc;
mod ipc_admin;
mod knowledge_admin;
mod prompt_folder;
pub use guide_persistence::{GuidePersistencePresenter, GuideSaveError};
pub use knowledge_admin::{GraphMaintenancePresenter, GraphReview};
pub use prompt_folder::{PromptFolderPresenter, PromptInventory};
mod presets;
mod releases;
mod runtime;
mod settings;
mod speech;

pub use characters::{
    CharacterDebugField, CharactersPresenter, MemoryCapacityPreview, MemoryListPage,
    MemoryListQuery, MemoryObserver, MemorySummary,
};
pub(crate) use chat::PresenterRunner;
pub use chat::{ChatModel, ChatPresenter, PresenterError, RequestView};
pub use connections::ConnectionsPresenter;
pub use conversation::{
    ConversationChange, ConversationChangePreview, ConversationError, ConversationHistoryPage,
    ConversationPresenter, ConversationSnapshot, RegeneratePreview,
};
pub(crate) use ipc::{PresenterAction, PresenterActionResult, PresenterProbe, PresenterSurface};
pub(crate) use ipc_admin::{PresenterRead, PresenterWrite};
pub use presets::{
    PresetParameterOption, PresetView, PresetsApi, PresetsPresenter, PresetsView, ProviderOption,
};
pub use releases::{
    ReleaseCommandError, ReleaseLoadState, ReleasesLifecycle, ReleasesPresenter, ReleasesSnapshot,
};
pub use runtime::RuntimeStatePresenter;
pub use settings::{
    AsrSettingsPresenter, BoundProperty, CharacterSettingsPresenter, CharactersSettingsPresenter,
    InterfaceSettingsPresenter, OnboardingSettingsPresenter, PresetSlot, PresetsSettingsPresenter,
    PropertyObserver, SettingsPresenter, TtsSettingsPresenter, VoiceSettingsPresenter,
};
pub use speech::SpeechSessionPresenter;

pub struct GuiPresenter {
    pub audio_input: AudioInputPresenter,
    pub prompts: Option<std::sync::Arc<crate::prompts::PromptCatalog>>,
    pub speech: SpeechSessionPresenter,
    pub ai_runtime: RuntimeStatePresenter,
    pub releases: ReleasesPresenter,
    pub connections: ConnectionsPresenter,
    pub conversation: ConversationPresenter,
    pub chat: ChatPresenter,
    pub settings: std::sync::Arc<SettingsPresenter>,
    pub presets: PresetsPresenter,
    pub characters: CharactersPresenter,
    pub graph: GraphMaintenancePresenter,
    pub guide_persistence: GuidePersistencePresenter,
    pub prompt_folder: PromptFolderPresenter,
    pub telegram_admin: Option<std::sync::Arc<crate::telegram_admin::TelegramAdmin>>,
    pub remote_voice_admin: Option<std::sync::Arc<crate::remote_voice::RemoteVoiceAdmin>>,
}
