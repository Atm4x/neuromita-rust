//! Explicit lexical retrieval over an immutable, character-scoped Timeline view.
//! The Timeline owns messages and visibility; this adapter owns no index or state.

use std::collections::BTreeMap;

use crate::{
    identity::CharacterId,
    timeline::{domain::MessageRole, view::MitaView},
};

use super::{
    memory_context::tokens,
    retrieval_fusion::{
        FusedHit, FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit,
        RetrievalKey, RetrievalMethod, RetrievalSource, fuse,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistorySearchConfig {
    pub limit: usize,
    pub max_content_chars: usize,
}

impl Default for HistorySearchConfig {
    fn default() -> Self {
        Self {
            limit: 5,
            max_content_chars: 1200,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySearchStatus {
    EmptyCorpus,
    NoMatches,
    Selected,
}

#[derive(Debug)]
pub struct HistorySearchResult {
    pub status: HistorySearchStatus,
    pub hits: Vec<FusedHit>,
    pub branch: crate::timeline::domain::BranchId,
    pub through: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistorySearchError {
    #[error("history view belongs to a different character")]
    WrongCharacter,
    #[error("invalid history search: {0}")]
    Configuration(&'static str),
    #[error("history retrieval evidence rejected: {0}")]
    Fusion(#[from] FusionError),
}

/// A source-backed, explicitly selected keyword path for older conversation
/// messages. `MitaView` has already applied witnesses, reset, hide, revision,
/// and branch-prefix semantics. Search does not inspect unrelated Timeline facts.
/// This is not FTS5 or vector retrieval and cannot silently replace either.
pub fn search_history(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
) -> Result<HistorySearchResult, HistorySearchError> {
    if view.character() != character {
        return Err(HistorySearchError::WrongCharacter);
    }
    if !(1..=32).contains(&config.limit) || !(1..=4000).contains(&config.max_content_chars) {
        return Err(HistorySearchError::Configuration("invalid result budget"));
    }
    if query.trim().is_empty() || query.chars().count() > 2048 {
        return Err(HistorySearchError::Configuration("invalid query"));
    }
    let terms = tokens(query);
    if terms.is_empty() {
        return Err(HistorySearchError::Configuration(
            "query has no searchable terms",
        ));
    }

    let messages = view
        .messages()
        .into_iter()
        .filter(|message| matches!(message.role, MessageRole::User | MessageRole::Assistant))
        .filter(|message| !message.content.trim().is_empty())
        .collect::<Vec<_>>();
    let corpus_empty = messages.is_empty();
    let mut ranked = messages
        .into_iter()
        .filter_map(|message| {
            let overlap = tokens(&message.content).intersection(&terms).count();
            (overlap > 0).then_some((overlap, message))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_overlap, left), (right_overlap, right)| {
        right_overlap
            .cmp(left_overlap)
            .then_with(|| right.learned_at.cmp(&left.learned_at))
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    let hits = ranked
        .into_iter()
        .take(config.limit)
        .map(|(overlap, message)| {
            let mut metadata = BTreeMap::new();
            metadata.insert("message_id".into(), message.message_id);
            metadata.insert("sequence".into(), message.sequence.to_string());
            metadata.insert("learned_at".into(), message.learned_at.to_string());
            metadata.insert("branch_id".into(), view.branch.0.to_string());
            metadata.insert("through".into(), view.through.to_string());
            metadata.insert(
                "role".into(),
                match message.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::System => unreachable!(),
                }
                .into(),
            );
            RetrievalHit {
                key: RetrievalKey {
                    character,
                    source: RetrievalSource::History,
                    id: message.event_id.0.to_string(),
                },
                content: message
                    .content
                    .chars()
                    .take(config.max_content_chars)
                    .collect(),
                raw_score: Some(overlap as f64),
                metadata,
            }
        })
        .collect::<Vec<_>>();
    let hits = fuse(
        character,
        &[RetrievalBucket {
            method: RetrievalMethod::Keyword,
            hits,
        }],
        FusionConfig {
            mode: FusionMode::Union,
            limit: config.limit,
            minimum_graph: 0,
        },
    )?;
    let status = if corpus_empty {
        HistorySearchStatus::EmptyCorpus
    } else if hits.is_empty() {
        HistorySearchStatus::NoMatches
    } else {
        HistorySearchStatus::Selected
    };
    Ok(HistorySearchResult {
        status,
        hits,
        branch: view.branch,
        through: view.through,
    })
}
