//! Generation-only composition of immutable, character-scoped lexical sources.
//! Neither the candidate selection nor the rendered context owns canonical facts.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    identity::CharacterId,
    memory::{MemoryRelation, MemoryState},
    timeline::{domain::MemoryRecord, view::MitaView},
};

use super::{
    history_generation::{HistoryEvidenceError, render_archived_history_evidence},
    history_retrieval::{HistorySearchConfig, HistorySearchResult, HistorySearchStatus},
    history_window::HistoryWindow,
    memory_context::{
        lexical_ranked_scoped, render_memory_islands, render_ranked_memory_context_scoped,
        render_relation_context, tokens,
    },
    retrieval_fusion::{
        FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit, RetrievalKey,
        RetrievalMethod, RetrievalSource, fuse,
    },
};

#[derive(Debug, Error)]
pub enum GenerationFusionError {
    #[error("generation retrieval no longer matches the admitted Timeline view")]
    StaleSelection,
    #[error("generation retrieval has invalid source evidence")]
    InvalidEvidence,
    #[error(transparent)]
    Fusion(#[from] FusionError),
    #[error(transparent)]
    History(#[from] HistoryEvidenceError),
}

/// Choose one bounded set of source-qualified candidates *before* rendering
/// Memory and archived History. This operation is used only for lexical
/// Keyword/FTS generation; semantic modes have a separate candidate pipeline.
pub(crate) fn select_lexical_generation(
    character: CharacterId,
    memories: &[MemoryRecord],
    islands: &[MemoryRecord],
    relations: &[MemoryRelation],
    view: &MitaView,
    window: &HistoryWindow,
    history: &HistorySearchResult,
    history_method: RetrievalMethod,
    query: &str,
    config: HistorySearchConfig,
) -> Result<Option<String>, GenerationFusionError> {
    if view.character() != character
        || !window.matches(view)
        || history.branch != view.branch
        || history.through != view.through
    {
        return Err(GenerationFusionError::StaleSelection);
    }
    if !matches!(
        history_method,
        RetrievalMethod::Keyword | RetrievalMethod::Fts
    ) || !(1..=8).contains(&config.limit)
        || !(64..=1200).contains(&config.max_content_chars)
    {
        return Err(GenerationFusionError::InvalidEvidence);
    }

    let searchable = tokens(query);
    let memory_hits = lexical_ranked_scoped(memories, query, false)
        .into_iter()
        .filter(|record| record.state == MemoryState::Active)
        .filter(|record| !tokens(&record.content).is_disjoint(&searchable))
        .map(|record| RetrievalHit {
            key: RetrievalKey {
                character,
                source: RetrievalSource::Memory,
                id: record.id.0.to_string(),
            },
            content: record.content.clone(),
            raw_score: None,
            metadata: BTreeMap::new(),
        })
        .collect::<Vec<_>>();
    let history_hits = history
        .hits
        .iter()
        .map(|hit| {
            if hit.key.character != character
                || hit.key.source != RetrievalSource::History
                || hit.evidence.len() != 1
                || hit.evidence[0].method != history_method
            {
                return Err(GenerationFusionError::InvalidEvidence);
            }
            Ok(RetrievalHit {
                key: hit.key.clone(),
                content: hit.content.clone(),
                raw_score: hit.evidence[0].raw_score,
                metadata: hit.metadata.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let buckets = [
        RetrievalBucket {
            method: RetrievalMethod::Keyword,
            hits: memory_hits,
        },
        RetrievalBucket {
            method: history_method,
            hits: history_hits,
        },
    ];
    let nonempty = buckets
        .into_iter()
        .filter(|bucket| !bucket.hits.is_empty())
        .collect::<Vec<_>>();
    let combined = fuse(
        character,
        &nonempty,
        FusionConfig {
            mode: FusionMode::Rrf { k: 60 },
            limit: config.limit,
            minimum_graph: 0,
        },
    )?;

    let (facts, _) = render_ranked_memory_context_scoped(
        combined
            .iter()
            .filter(|hit| hit.key.source == RetrievalSource::Memory)
            .filter_map(|hit| {
                memories.iter().find(|record| {
                    record.id.0.to_string() == hit.key.id && record.content == hit.content
                })
            }),
        false,
    );
    let chosen_history = combined
        .into_iter()
        .filter(|hit| hit.key.source == RetrievalSource::History)
        .collect::<Vec<_>>();
    let history_result = HistorySearchResult {
        status: if chosen_history.is_empty() {
            HistorySearchStatus::NoMatches
        } else {
            HistorySearchStatus::Selected
        },
        hits: chosen_history,
        branch: history.branch,
        through: history.through,
        embedding_space: None,
        cost: None,
    };
    let history_text = render_archived_history_evidence(
        character,
        view,
        window,
        &history_result,
        config.limit,
        config.max_content_chars,
    )?;
    let mut sections = Vec::new();
    for section in [
        render_memory_islands(islands),
        facts,
        render_relation_context(relations, query),
        history_text,
    ] {
        if let Some(section) = section {
            sections.push(section);
        }
    }
    Ok((!sections.is_empty()).then(|| sections.join("\n")))
}
