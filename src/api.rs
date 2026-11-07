use std::sync::Arc;

use tokio::sync::watch;

use crate::{
    character::{CharacterError, CharacterHandle, CharacterId},
    generation::{PresetCatalogError, PresetId, PresetRecord},
    interaction::UserInput,
    request::{
        DiagnosticScope, DiagnosticsAccess, GenerationCommand, InitiateRequest, ReactRequest,
        ReplyRequest, RequestDiscovery, RequestHandle, RequestId, SubmitError,
    },
    runtime::{self, ApiUnavailable},
    settings::{
        SectionSnapshot, SettingsApi, SettingsError, SettingsImportFormat, SettingsOwner,
        SettingsSection, SettingsSnapshot, SettingsTransfer, SettingsTransferError,
    },
    timeline::EventId,
};

pub struct CharacterApi;
impl CharacterApi {
    pub fn get(id: CharacterId) -> Result<CharacterHandle, CharacterError> {
        runtime::current()
            .ok()
            .and_then(|bindings| bindings.characters.get(id))
            .ok_or(CharacterError::Unavailable)
    }
}

pub struct PresetApi;

impl PresetApi {
    fn catalog() -> Result<Arc<crate::generation::PresetCatalog>, PresetCatalogError> {
        runtime::current()
            .map(|bindings| Arc::clone(&bindings.presets))
            .map_err(|_| PresetCatalogError::Unavailable)
    }

    pub fn list() -> Result<Vec<(PresetId, PresetRecord)>, PresetCatalogError> {
        Ok(Self::catalog()?.list())
    }

    pub fn create(record: PresetRecord) -> Result<PresetId, PresetCatalogError> {
        Self::catalog()?.create(record)
    }

    pub fn update(id: PresetId, record: PresetRecord) -> Result<(), PresetCatalogError> {
        Self::catalog()?.update(id, record)
    }

    pub fn delete(id: PresetId) -> Result<(), PresetCatalogError> {
        Self::catalog()?.delete(id)
    }
}

/// Explicit read-only entrypoint for user-initiated inspection of forgotten facts.
/// This action never submits a model generation request or restores memory.
pub struct HistoricalRecallApi;

#[derive(Debug, thiserror::Error)]
pub enum HistoricalRecallApiError {
    #[error("application is unavailable")]
    Unavailable,
    #[error("character is unavailable")]
    CharacterUnavailable,
    #[error(transparent)]
    Recall(#[from] crate::app::historical_recall::ExplicitRecallError),
}

impl HistoricalRecallApi {
    /// Call only from an authenticated/locally trusted explicit user action.
    /// This entrypoint does not itself authenticate the caller.
    pub async fn inspect(
        character: CharacterId,
        selector: crate::knowledge::forgotten_recall::RecallSelector,
        query: &str,
    ) -> Result<Option<String>, HistoricalRecallApiError> {
        let bindings = runtime::current().map_err(|_| HistoricalRecallApiError::Unavailable)?;
        let owner = bindings
            .characters
            .get(character)
            .ok_or(HistoricalRecallApiError::CharacterUnavailable)?;
        crate::app::historical_recall::recall_from_character(
            &bindings.knowledge,
            &owner,
            selector,
            query,
        )
        .await
        .map_err(Into::into)
    }
}

/// Local, explicit history inspection. This API does not authenticate remote clients.
/// It never passes retrieved history to ordinary generation.
pub struct HistorySearchApi;

#[derive(Debug, thiserror::Error)]
pub enum HistorySearchApiError {
    #[error("application is unavailable")]
    Unavailable,
    #[error("character is unavailable")]
    CharacterUnavailable,
    #[error(transparent)]
    Character(#[from] CharacterError),
    #[error(transparent)]
    Search(#[from] crate::knowledge::history_retrieval::HistorySearchError),
    #[error("history selection changed during inspection")]
    StaleSelection,
}

enum HistorySearchSelection {
    Keyword,
    Fts,
    Semantic {
        fusion: crate::knowledge::retrieval_fusion::FusionMode,
        minimum_cosine: Option<f64>,
    },
}

impl HistorySearchApi {
    /// Only invoke from an explicitly authorized local UI or equivalent boundary.
    pub async fn inspect(
        character: CharacterId,
        query: &str,
    ) -> Result<crate::knowledge::history_retrieval::HistorySearchResult, HistorySearchApiError>
    {
        Self::inspect_selected(character, query, HistorySearchSelection::Keyword).await
    }

    /// Explicit SQLite FTS5 history inspection over a single live Timeline
    /// projection. Missing FTS5 is an error, never a keyword fallback.
    pub async fn inspect_fts(
        character: CharacterId,
        query: &str,
    ) -> Result<crate::knowledge::history_retrieval::HistorySearchResult, HistorySearchApiError>
    {
        Self::inspect_selected(character, query, HistorySearchSelection::Fts).await
    }

    /// Explicit semantic History mode; requires the application's configured
    /// embedding provider. No lexical fallback if that provider is unavailable.
    /// This is a local trusted-user API, not an authenticated remote endpoint.
    pub async fn inspect_semantic(
        character: CharacterId,
        query: &str,
        fusion: crate::knowledge::retrieval_fusion::FusionMode,
        minimum_cosine: Option<f64>,
    ) -> Result<crate::knowledge::history_retrieval::HistorySearchResult, HistorySearchApiError>
    {
        Self::inspect_selected(
            character,
            query,
            HistorySearchSelection::Semantic {
                fusion,
                minimum_cosine,
            },
        )
        .await
    }

    async fn inspect_selected(
        character: CharacterId,
        query: &str,
        selection: HistorySearchSelection,
    ) -> Result<crate::knowledge::history_retrieval::HistorySearchResult, HistorySearchApiError>
    {
        let bindings = runtime::current().map_err(|_| HistorySearchApiError::Unavailable)?;
        let owner = bindings
            .characters
            .get(character)
            .ok_or(HistorySearchApiError::CharacterUnavailable)?;
        let captured = owner.memories().await?;
        let context = owner.get_context(captured.anchor()).await?;
        if context.view.character() != character
            || context.view.branch.0 != captured.anchor().branch_id
            || context.view.through != captured.anchor().through
        {
            return Err(HistorySearchApiError::StaleSelection);
        }
        let selected = match selection {
            HistorySearchSelection::Keyword => crate::knowledge::history_retrieval::search_history(
                character,
                &context.view,
                query,
                Default::default(),
            )?,
            HistorySearchSelection::Fts => crate::knowledge::history_retrieval::search_history_fts(
                character,
                &context.view,
                query,
                Default::default(),
            )?,
            HistorySearchSelection::Semantic {
                fusion,
                minimum_cosine,
            } => {
                bindings
                    .knowledge
                    .search_history_semantic(
                        character,
                        &context.view,
                        query,
                        Default::default(),
                        fusion,
                        minimum_cosine,
                    )
                    .await?
            }
        };
        let fresh = owner.memories().await?;
        if captured.anchor() != fresh.anchor() || !captured.matches_version(&fresh) {
            return Err(HistorySearchApiError::StaleSelection);
        }
        Ok(selected)
    }
}

/// Trusted-local knowledge cache inspection and confirmed derived-data clear.
/// The API does not authenticate remote clients and must not be exported there.
pub struct KnowledgeCacheApi;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KnowledgeCacheApiError {
    #[error("application is unavailable")]
    Unavailable,
    #[error("semantic embedding cache is not configured")]
    BackendUnavailable,
    #[error(transparent)]
    Clear(#[from] crate::knowledge::KnowledgeCacheClearError),
}

impl KnowledgeCacheApi {
    /// A preview can be displayed to a local user without leaking vector content.
    pub fn preview()
    -> Result<crate::knowledge::semantic_memory::EmbeddingCachePreview, KnowledgeCacheApiError>
    {
        let bindings = runtime::current().map_err(|_| KnowledgeCacheApiError::Unavailable)?;
        bindings
            .knowledge
            .preview_embedding_cache_clear()
            .ok_or(KnowledgeCacheApiError::BackendUnavailable)
    }

    /// Clear only after an explicit confirmation tied to the original preview.
    /// The canonical Timeline is never mutated by this operation.
    pub fn clear_confirmed(
        preview: &crate::knowledge::semantic_memory::EmbeddingCachePreview,
        confirmed: bool,
    ) -> Result<usize, KnowledgeCacheApiError> {
        let bindings = runtime::current().map_err(|_| KnowledgeCacheApiError::Unavailable)?;
        bindings
            .knowledge
            .clear_embedding_cache_confirmed(preview, confirmed)
            .map_err(Into::into)
    }
}

/// Explicit local graph maintenance against the canonical character memory owner.
/// The caller is responsible for local-user authorization; no remote route is exposed.
pub struct GraphMaintenanceApi;

#[derive(Debug, thiserror::Error)]
pub enum GraphMaintenanceApiError {
    #[error("application is unavailable")]
    Unavailable,
    #[error("character is unavailable")]
    CharacterUnavailable,
    #[error("graph preview belongs to another character")]
    ForeignPreview,
    #[error(transparent)]
    Character(#[from] CharacterError),
    #[error(transparent)]
    Maintenance(#[from] crate::knowledge::maintenance::GraphMaintenanceError),
}

impl GraphMaintenanceApi {
    /// Report the running application's explicit typed GraphExtractor capability.
    /// Unavailable is not a request to fall back to lexical reconstruction.
    pub fn reindex_available() -> Result<bool, GraphMaintenanceApiError> {
        let bindings = runtime::current().map_err(|_| GraphMaintenanceApiError::Unavailable)?;
        Ok(bindings.graph_extractor.is_some())
    }

    /// Capture a read-only preview of exact source IDs at a caller-supplied clock time.
    pub async fn preview(
        character: CharacterId,
        ids: &[crate::memory::MemoryId],
        as_of_unix_ms: u64,
        mode: crate::knowledge::maintenance::GraphMaintenanceMode,
    ) -> Result<crate::knowledge::maintenance::GraphMaintenancePreview, GraphMaintenanceApiError>
    {
        let bindings = runtime::current().map_err(|_| GraphMaintenanceApiError::Unavailable)?;
        let owner = bindings
            .characters
            .get(character)
            .ok_or(GraphMaintenanceApiError::CharacterUnavailable)?;
        let snapshot = owner.memories().await?;
        crate::knowledge::maintenance::preview_graph_maintenance(snapshot, ids, as_of_unix_ms, mode)
            .map_err(Into::into)
    }

    /// Apply one confirmed, version-checked batch using the running application's
    /// explicitly configured typed extractor. Missing backends remain unavailable.
    pub async fn apply_confirmed(
        character: CharacterId,
        plan: crate::knowledge::maintenance::GraphMaintenancePreview,
        confirmed: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<crate::character::MemoryEditResult, GraphMaintenanceApiError> {
        if plan.character() != character {
            return Err(GraphMaintenanceApiError::ForeignPreview);
        }
        let bindings = runtime::current().map_err(|_| GraphMaintenanceApiError::Unavailable)?;
        let owner = bindings
            .characters
            .get(character)
            .ok_or(GraphMaintenanceApiError::CharacterUnavailable)?;
        crate::knowledge::maintenance::apply_graph_maintenance(
            &owner,
            plan,
            bindings.graph_extractor.as_deref(),
            confirmed,
            cancel,
        )
        .await
        .map_err(Into::into)
    }
}

pub struct ChatApi;

impl ChatApi {
    pub fn publish_conversation_message(
        message: crate::timeline::conversation::ConversationMessage,
    ) -> Result<crate::identity::EventId, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .publish_conversation_message(message)
    }

    pub fn publish_message(
        character: CharacterId,
        input: UserInput,
    ) -> Result<EventId, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .publish_message(character, input)
    }

    pub fn reply(request: ReplyRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    pub fn react(request: ReactRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    pub fn initiate(request: InitiateRequest) -> Result<RequestHandle, SubmitError> {
        Self::submit(request)
    }
    fn submit(request: impl GenerationCommand + 'static) -> Result<RequestHandle, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .send(request)
    }
    pub fn observe_requests() -> Result<RequestDiscovery, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .observe_requests()
    }
    pub fn active_request(id: RequestId) -> Result<Option<RequestHandle>, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .active_request(id)
    }
    pub fn trace(id: RequestId) -> Result<Option<Arc<crate::request::RequestTrace>>, SubmitError> {
        Ok(runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .registry
            .trace(id))
    }

    /// Read diagnostics for one character. Pass an owner scope when caller
    /// identity matters; a missing request is reported, never replaced by a
    /// different trace.
    pub fn diagnostics(character: CharacterId) -> Result<DiagnosticsAccess, SubmitError> {
        Self::diagnostics_for(DiagnosticScope::character(character))
    }

    pub fn diagnostics_for(scope: DiagnosticScope) -> Result<DiagnosticsAccess, SubmitError> {
        runtime::current()
            .map_err(SubmitError::from)?
            .chat
            .diagnostics(scope)
    }
}

impl From<ApiUnavailable> for SubmitError {
    fn from(error: ApiUnavailable) -> Self {
        match error {
            ApiUnavailable::Uninitialized => Self::Uninitialized,
            ApiUnavailable::Stopped => Self::Stopped,
        }
    }
}

impl SettingsApi {
    fn owner() -> Result<Arc<SettingsOwner>, SettingsError> {
        Ok(Arc::clone(
            &runtime::current().map_err(SettingsError::from)?.settings,
        ))
    }
    pub fn snapshot<T: SettingsSection>() -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.snapshot()
    }
    pub fn snapshot_all() -> Result<Arc<SettingsSnapshot>, SettingsError> {
        Self::owner()?.snapshot_all()
    }
    pub fn observe() -> Result<watch::Receiver<Arc<SettingsSnapshot>>, SettingsError> {
        Self::owner()?.observe()
    }
    /// Pure preparation, also available before the Core is started.
    pub fn prepare_import(
        format: SettingsImportFormat,
        bytes: &[u8],
    ) -> Result<SettingsTransfer, SettingsTransferError> {
        SettingsTransfer::prepare(format, bytes)
    }
    pub fn export() -> Result<SettingsTransfer, SettingsError> {
        Self::owner()?.export()
    }
    pub fn import(
        expected_revision: u64,
        transfer: SettingsTransfer,
    ) -> Result<Arc<SettingsSnapshot>, SettingsError> {
        Self::owner()?.import(expected_revision, transfer)
    }
    pub fn replace<T: SettingsSection>(
        expected_revision: u64,
        value: T,
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.replace(expected_revision, value)
    }
    /// The edit must be short and must not block or re-enter SettingsApi.
    pub fn edit<T: SettingsSection>(
        edit: impl FnOnce(&mut T),
    ) -> Result<SectionSnapshot<T>, SettingsError> {
        Self::owner()?.edit(edit)
    }
    pub async fn flush() -> Result<(), SettingsError> {
        Self::owner()?.flush().await
    }
    pub fn retry_save() -> Result<(), SettingsError> {
        Self::owner()?.request_save()
    }
}

impl From<ApiUnavailable> for SettingsError {
    fn from(error: ApiUnavailable) -> Self {
        match error {
            ApiUnavailable::Uninitialized => Self::Uninitialized,
            ApiUnavailable::Stopped => Self::Stopped,
        }
    }
}
