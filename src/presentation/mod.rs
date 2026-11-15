pub mod ai_hub_catalog;
mod ai_install;
pub mod ai_install_operation;
pub use ai_install::{AiHubInstallPresenter, AiHubInstallView};
pub use native_hub::NativeHubApi;
pub mod native_download;
pub mod native_hub;
pub mod native_model;
pub mod native_release_catalog;
pub mod native_revisions;
pub use ai_install_operation::AiInstallOperationPresenter;
mod audio_input;
pub mod character_debug;
mod characters;
mod chess;
pub use audio_input::AudioInputPresenter;
pub use character_debug::project_character_debug;
mod chat;
mod connections;
mod conversation;
mod guide_persistence;
pub mod hardware;
mod ipc;
mod ipc_admin;
mod knowledge_admin;
mod prompt_folder;
mod selection;
pub use guide_persistence::{GuidePersistencePresenter, GuideSaveError};
pub use knowledge_admin::{GraphMaintenancePresenter, GraphReview};
pub use prompt_folder::{PromptFolderPresenter, PromptInventory};
pub use selection::{ChatSelection, ChatSelectionPresenter};
mod presets;
mod remote_voice;
mod telegram;
pub use remote_voice::RemoteVoicePresenter;
pub use telegram::TelegramPresenter;
mod releases;
mod runtime;
mod settings;
mod speech;
mod speech_chat;
pub use speech_chat::{SpeechChatPresenter, SpeechDraftSnapshot};

pub use characters::{
    CharacterDebugField, CharactersPresenter, MemoryCapacityPreview, MemoryListPage,
    MemoryListQuery, MemoryObserver, MemorySummary,
};
pub(crate) use chat::PresenterRunner;
pub use chat::{ChatModel, ChatPresenter, PresenterError, RequestView};
pub use chess::ChessAction;
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
    pub ai_install: Option<AiHubInstallPresenter>,
    pub native_hub: Option<NativeHubApi>,
    pub selection: std::sync::Arc<ChatSelectionPresenter>,
    pub audio_input: AudioInputPresenter,
    pub prompts: Option<std::sync::Arc<crate::prompts::PromptCatalog>>,
    pub speech: SpeechSessionPresenter,
    pub speech_chat: SpeechChatPresenter,
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
    pub telegram: Option<TelegramPresenter>,
    pub remote_voice: Option<RemoteVoicePresenter>,
}
