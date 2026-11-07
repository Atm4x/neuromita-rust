//! Explicit read-only historical-memory action, orchestrating the character owner
//! and the independent Knowledge retrieval layer without a second store.

use crate::{
    character::{CharacterError, CharacterHandle},
    knowledge::{
        HistoricalMemorySelectionError, KnowledgeRetrieval,
        forgotten_recall::{RecallError, RecallSelector, recall_forgotten_records},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum ExplicitRecallError {
    #[error("character owner cannot provide an authoritative memory snapshot: {0}")]
    Character(#[from] CharacterError),
    #[error("invalid explicit forgotten-memory selector: {0}")]
    Selector(#[from] RecallError),
    #[error(transparent)]
    Retrieval(#[from] HistoricalMemorySelectionError),
}

/// Read-only historical recall, to be invoked only by an explicitly authorized
/// user action. This orchestrator captures an authoritative owner snapshot,
/// delegates immutable selection to Knowledge and re-reads the owner after await.
/// It is not itself an authentication or user-consent primitive.
pub async fn recall_from_character(
    knowledge: &KnowledgeRetrieval,
    owner: &CharacterHandle,
    selector: RecallSelector,
    query: &str,
) -> Result<Option<String>, ExplicitRecallError> {
    let captured = owner.memories().await?;
    let recalled = recall_forgotten_records(
        captured.character(),
        captured.anchor(),
        captured.records(),
        selector,
    )?;
    let pending = knowledge
        .select_recalled_forgotten(&captured, &recalled, query)
        .await?;
    let fresh = owner.memories().await?;
    Ok(pending.finalize(&fresh)?)
}
