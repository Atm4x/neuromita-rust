//! Generation-only composition of immutable, character-scoped retrieval sources.
//! Neither the candidate selection nor the rendered context owns canonical facts.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    identity::CharacterId,
    memory::{MemoryKind, MemoryRelation, MemoryState},
    timeline::{domain::MemoryRecord, view::MitaView},
};

use super::{
    embedding::{EmbeddingError, EmbeddingSpace},
    history_generation::{HistoryEvidenceError, render_archived_history_evidence},
    history_retrieval::{
        self, HistorySearchConfig, HistorySearchError, HistorySearchResult, HistorySearchStatus,
    },
    history_window::HistoryWindow,
    memory_context::{
        priority_rank, render_memory_islands, render_ranked_memory_context_scoped,
        render_relation_context, tokens,
    },
    retrieval_fusion::{
        FusedHit, FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit,
        RetrievalKey, RetrievalMethod, RetrievalSource, fuse,
    },
    scoped_fts::{ScopedFtsError, rank as rank_scoped_fts},
    semantic_memory::{RetrievalCost, SemanticMemoryRetriever},
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
    #[error(transparent)]
    HistorySearch(#[from] HistorySearchError),
    #[error(transparent)]
    Embedding(#[from] EmbeddingError),
    #[error(transparent)]
    Fts(#[from] ScopedFtsError),
}

fn active_retrieval_facts(memories: &[MemoryRecord]) -> impl Iterator<Item = &MemoryRecord> {
    memories.iter().filter(|record| {
        record.state == MemoryState::Active
            && matches!(record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
    })
}

/// Query-matched, source-owned fact admission for generation-only Keyword buckets.
/// Ordinary Off-mode lexical memory selection keeps its separate Fact-only contract.
fn keyword_generation_facts<'a>(
    memories: &'a [MemoryRecord],
    query: &str,
) -> Vec<&'a MemoryRecord> {
    let terms = tokens(query);
    let mut ranked = active_retrieval_facts(memories)
        .filter_map(|record| {
            let overlap = tokens(&record.content).intersection(&terms).count();
            (overlap > 0).then_some((record, overlap))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(a, a_overlap), (b, b_overlap)| {
        b_overlap
            .cmp(a_overlap)
            .then_with(|| priority_rank(b.priority).cmp(&priority_rank(a.priority)))
            .then_with(|| a.id.cmp(&b.id))
    });
    ranked.into_iter().map(|(record, _)| record).collect()
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

    let memory_hits = match history_method {
        RetrievalMethod::Keyword => keyword_generation_facts(memories, query)
            .into_iter()
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
            .collect::<Vec<_>>(),
        RetrievalMethod::Fts => {
            let scoped = active_retrieval_facts(memories).collect::<Vec<_>>();
            let content = scoped
                .iter()
                .map(|record| record.content.as_str())
                .collect::<Vec<_>>();
            rank_scoped_fts(query, &content)?
                .into_iter()
                .map(|(index, score)| {
                    let record = scoped[index];
                    RetrievalHit {
                        key: RetrievalKey {
                            character,
                            source: RetrievalSource::Memory,
                            id: record.id.0.to_string(),
                        },
                        content: record.content.clone(),
                        raw_score: Some(score),
                        metadata: BTreeMap::new(),
                    }
                })
                .collect::<Vec<_>>()
        }
        _ => return Err(GenerationFusionError::InvalidEvidence),
    };
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
            method: history_method,
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

    render_generation_selection(
        character, memories, islands, relations, view, window, query, config, combined, None,
    )
}

/// Single semantic admission: one embedding query, one cross-source RRF, and one
/// reranker. Only snapshot-owned Memory facts and archived Timeline messages may
/// enter; subsequent rendering independently validates exact event provenance.
pub(crate) async fn select_semantic_generation(
    retriever: &SemanticMemoryRetriever,
    memory_mode: FusionMode,
    character: CharacterId,
    memories: &[MemoryRecord],
    islands: &[MemoryRecord],
    relations: &[MemoryRelation],
    view: &MitaView,
    window: &HistoryWindow,
    query: &str,
    config: HistorySearchConfig,
    history_fusion: bool,
    turn: crate::timeline::selection::TimelineTurn,
) -> Result<Option<String>, GenerationFusionError> {
    if view.character() != character || !window.matches(view) {
        return Err(GenerationFusionError::StaleSelection);
    }
    history_retrieval::validate(character, view, query, config)?;
    if config.limit > 8 || config.max_content_chars > 1200 {
        return Err(GenerationFusionError::InvalidEvidence);
    }
    let facts = active_retrieval_facts(memories).collect::<Vec<_>>();
    let messages =
        history_retrieval::scoped_candidates(view, config.max_content_chars, Some(window))?;
    if facts.len() + messages.len() > 1024 {
        return Err(GenerationFusionError::Embedding(
            EmbeddingError::Configuration(
                "combined semantic source exceeds bounded per-query capacity".into(),
            ),
        ));
    }
    let admitted = facts
        .iter()
        .map(|record| (*record).clone())
        .collect::<Vec<_>>();
    let (space, memory_scores, history_scores, cost) = retriever
        .score_joint_generation(character, &admitted, &messages, query, turn)
        .await?;
    let mut ranked_facts = facts.iter().copied().zip(memory_scores).collect::<Vec<_>>();
    ranked_facts.sort_by(|(a, a_score), (b, b_score)| {
        b_score
            .total_cmp(a_score)
            .then_with(|| priority_rank(b.priority).cmp(&priority_rank(a.priority)))
            .then_with(|| a.id.cmp(&b.id))
    });
    let lexical = keyword_generation_facts(&admitted, query)
        .into_iter()
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
    let matched = lexical
        .iter()
        .map(|hit| hit.key.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let memory_vectors = ranked_facts
        .iter()
        .filter(|(record, _)| match memory_mode {
            FusionMode::Intersect { min_methods, .. } if min_methods >= 3 => false,
            FusionMode::Intersect { min_methods: 2, .. } => {
                let id = record.id.0.to_string();
                matched.contains(id.as_str())
            }
            _ => true,
        })
        .map(|(record, score)| RetrievalHit {
            key: RetrievalKey {
                character,
                source: RetrievalSource::Memory,
                id: record.id.0.to_string(),
            },
            content: record.content.clone(),
            raw_score: Some(*score),
            metadata: BTreeMap::new(),
        })
        .collect::<Vec<_>>();
    let mut ranked_history = messages.iter().zip(history_scores).collect::<Vec<_>>();
    ranked_history.sort_by(|(a, a_score), (b, b_score)| {
        b_score
            .total_cmp(a_score)
            .then_with(|| b.learned_at.cmp(&a.learned_at))
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    let history_vectors = ranked_history
        .iter()
        .map(|(message, score)| history_retrieval::hit(character, view, message, *score))
        .collect::<Vec<_>>();
    let mut buckets = vec![
        RetrievalBucket {
            method: RetrievalMethod::Vector,
            hits: memory_vectors,
        },
        RetrievalBucket {
            method: RetrievalMethod::Vector,
            hits: history_vectors,
        },
    ];
    if matches!(
        memory_mode,
        FusionMode::Union | FusionMode::Rrf { .. } | FusionMode::Intersect { .. }
    ) {
        let lexical = if matches!(
            memory_mode,
            FusionMode::Intersect {
                min_methods: 3..,
                ..
            }
        ) {
            Vec::new()
        } else {
            lexical
        };
        buckets.push(RetrievalBucket {
            method: RetrievalMethod::Keyword,
            hits: lexical,
        });
    }
    if history_fusion {
        buckets.push(history_retrieval::lexical_bucket(
            character, view, &messages, query,
        ));
    }
    let nonempty = buckets
        .into_iter()
        .filter(|bucket| !bucket.hits.is_empty())
        .collect::<Vec<_>>();
    let mut combined = fuse(
        character,
        &nonempty,
        FusionConfig {
            mode: FusionMode::Rrf {
                k: match memory_mode {
                    FusionMode::Rrf { k } => k,
                    _ => 60,
                },
            },
            limit: config.limit.max(retriever.rerank_candidate_limit()),
            minimum_graph: 0,
        },
    )?;
    retriever
        .rerank_fused(query, &mut combined, Some(turn))
        .await?;
    combined.truncate(config.limit);
    render_generation_selection(
        character,
        memories,
        islands,
        relations,
        view,
        window,
        query,
        config,
        combined,
        Some((space, cost)),
    )
}

fn render_generation_selection(
    character: CharacterId,
    memories: &[MemoryRecord],
    islands: &[MemoryRecord],
    relations: &[MemoryRelation],
    view: &MitaView,
    window: &HistoryWindow,
    query: &str,
    config: HistorySearchConfig,
    selected: Vec<FusedHit>,
    semantic: Option<(EmbeddingSpace, RetrievalCost)>,
) -> Result<Option<String>, GenerationFusionError> {
    if view.character() != character || !window.matches(view) {
        return Err(GenerationFusionError::StaleSelection);
    }
    if selected.len() > config.limit || config.limit > 8 || config.max_content_chars > 1200 {
        return Err(GenerationFusionError::InvalidEvidence);
    }
    let mut chosen_memory = Vec::new();
    let mut chosen_history = Vec::new();
    for hit in selected {
        if hit.key.character != character {
            return Err(GenerationFusionError::InvalidEvidence);
        }
        match hit.key.source {
            RetrievalSource::Memory => {
                let Some(record) = memories.iter().find(|record| {
                    record.id.0.to_string() == hit.key.id
                        && record.content == hit.content
                        && matches!(record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
                        && record.state == MemoryState::Active
                }) else {
                    return Err(GenerationFusionError::InvalidEvidence);
                };
                chosen_memory.push(record);
            }
            RetrievalSource::History => chosen_history.push(hit),
            _ => return Err(GenerationFusionError::InvalidEvidence),
        }
    }
    let (memory_text, _) = render_ranked_memory_context_scoped(chosen_memory.into_iter(), false);
    let (space, cost) = match semantic {
        Some((space, cost)) => (Some(space), Some(cost)),
        None => (None, None),
    };
    let history = HistorySearchResult {
        status: if chosen_history.is_empty() {
            HistorySearchStatus::NoMatches
        } else {
            HistorySearchStatus::Selected
        },
        hits: chosen_history,
        branch: view.branch,
        through: view.through,
        embedding_space: space,
        cost,
    };
    let history_text = render_archived_history_evidence(
        character,
        view,
        window,
        &history,
        config.limit,
        config.max_content_chars,
    )?;
    let sections = [
        render_memory_islands(islands),
        memory_text,
        render_relation_context(relations, query),
        history_text,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    Ok((!sections.is_empty()).then(|| sections.join("\n")))
}
