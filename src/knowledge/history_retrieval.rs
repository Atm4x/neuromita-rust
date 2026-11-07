//! Explicit retrieval over one immutable, character-scoped Timeline view.
//! History membership, visibility and revisions come exclusively from MitaView.

use std::collections::BTreeMap;

use crate::{
    identity::CharacterId,
    timeline::{
        domain::{MessageRole, ProjectedMessage},
        view::MitaView,
    },
};

use super::{
    embedding::{EmbeddingError, EmbeddingSpace},
    history_window::HistoryWindow,
    memory_context::tokens,
    retrieval_fusion::{
        FusedHit, FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit,
        RetrievalKey, RetrievalMethod, RetrievalSource, fuse,
    },
    semantic_memory::{RetrievalCost, SemanticMemoryRetriever},
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
    /// None for lexical-only retrieval; explicit for every semantic result.
    pub embedding_space: Option<EmbeddingSpace>,
    pub cost: Option<RetrievalCost>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistorySearchError {
    #[error("history window does not match the selected Timeline prefix")]
    StaleWindow,
    #[error("history view belongs to a different character")]
    WrongCharacter,
    #[error("invalid history search: {0}")]
    Configuration(&'static str),
    #[error("semantic history retrieval is not configured")]
    BackendUnavailable,
    #[error("SQLite FTS5 backend is not available")]
    FtsUnavailable,
    #[error("SQLite FTS5 derived retrieval failed: {0}")]
    FtsFailure(&'static str),
    #[error("history corpus contains {found} candidates; maximum is {maximum}")]
    CorpusLimit { found: usize, maximum: usize },
    #[error("history retrieval evidence rejected: {0}")]
    Fusion(#[from] FusionError),
    #[error("semantic history provider failed: {0}")]
    Embedding(#[from] EmbeddingError),
}

pub(crate) fn validate(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
) -> Result<(), HistorySearchError> {
    if view.character() != character {
        return Err(HistorySearchError::WrongCharacter);
    }
    if !(1..=32).contains(&config.limit) || !(1..=4000).contains(&config.max_content_chars) {
        return Err(HistorySearchError::Configuration("invalid result budget"));
    }
    if query.trim().is_empty() || query.chars().count() > 2048 {
        return Err(HistorySearchError::Configuration("invalid query"));
    }
    if tokens(query).is_empty() {
        return Err(HistorySearchError::Configuration(
            "query has no searchable terms",
        ));
    }
    Ok(())
}

fn candidates(view: &MitaView, max_content_chars: usize) -> Vec<ProjectedMessage> {
    view.messages()
        .into_iter()
        .filter(|message| matches!(message.role, MessageRole::User | MessageRole::Assistant))
        .filter_map(|mut message| {
            message.content = message.content.chars().take(max_content_chars).collect();
            (!message.content.trim().is_empty()).then_some(message)
        })
        .collect()
}

pub(crate) fn scoped_candidates(
    view: &MitaView,
    max_content_chars: usize,
    window: Option<&HistoryWindow>,
) -> Result<Vec<ProjectedMessage>, HistorySearchError> {
    if let Some(window) = window {
        if !window.matches(view) {
            return Err(HistorySearchError::StaleWindow);
        }
    }
    Ok(candidates(view, max_content_chars)
        .into_iter()
        .filter(|message| window.is_none_or(|window| window.is_archived(message.event_id)))
        .collect())
}

pub(crate) fn hit(
    character: CharacterId,
    view: &MitaView,
    message: &ProjectedMessage,
    score: f64,
) -> RetrievalHit {
    let mut metadata = BTreeMap::new();
    metadata.insert("message_id".into(), message.message_id.clone());
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
        content: message.content.clone(),
        raw_score: Some(score),
        metadata,
    }
}

pub(crate) fn lexical_bucket(
    character: CharacterId,
    view: &MitaView,
    messages: &[ProjectedMessage],
    query: &str,
) -> RetrievalBucket {
    let terms = tokens(query);
    let mut ranked = messages
        .iter()
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
    RetrievalBucket {
        method: RetrievalMethod::Keyword,
        hits: ranked
            .into_iter()
            .take(1024)
            .map(|(overlap, message)| hit(character, view, message, overlap as f64))
            .collect(),
    }
}

fn finish(
    view: &MitaView,
    corpus_empty: bool,
    hits: Vec<FusedHit>,
    embedding_space: Option<EmbeddingSpace>,
    cost: Option<RetrievalCost>,
) -> HistorySearchResult {
    let status = if corpus_empty {
        HistorySearchStatus::EmptyCorpus
    } else if hits.is_empty() {
        HistorySearchStatus::NoMatches
    } else {
        HistorySearchStatus::Selected
    };
    HistorySearchResult {
        status,
        hits,
        branch: view.branch,
        through: view.through,
        embedding_space,
        cost,
    }
}

/// Explicit local keyword selection; does not invoke embeddings or change memory.
pub fn search_history(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
) -> Result<HistorySearchResult, HistorySearchError> {
    search_history_with_window(character, view, query, config, None)
}

pub fn search_archived_history(
    character: CharacterId,
    view: &MitaView,
    window: &HistoryWindow,
    query: &str,
    config: HistorySearchConfig,
) -> Result<HistorySearchResult, HistorySearchError> {
    search_history_with_window(character, view, query, config, Some(window))
}

fn search_history_with_window(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
    window: Option<&HistoryWindow>,
) -> Result<HistorySearchResult, HistorySearchError> {
    validate(character, view, query, config)?;
    let messages = scoped_candidates(view, config.max_content_chars, window)?;
    let hits = fuse(
        character,
        &[lexical_bucket(character, view, &messages, query)],
        FusionConfig {
            mode: FusionMode::Union,
            limit: config.limit,
            minimum_graph: 0,
        },
    )?;
    Ok(finish(view, messages.is_empty(), hits, None, None))
}

/// Ephemeral FTS5 source over one already-authorized MitaView. This index is
/// populated from the canonical Timeline projection for this read, and is
/// never persisted. SQLite FTS5 is required: there is no keyword fallback.
pub fn search_history_fts(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
) -> Result<HistorySearchResult, HistorySearchError> {
    search_history_fts_with_window(character, view, query, config, None)
}

pub fn search_archived_history_fts(
    character: CharacterId,
    view: &MitaView,
    window: &HistoryWindow,
    query: &str,
    config: HistorySearchConfig,
) -> Result<HistorySearchResult, HistorySearchError> {
    search_history_fts_with_window(character, view, query, config, Some(window))
}

fn search_history_fts_with_window(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
    window: Option<&HistoryWindow>,
) -> Result<HistorySearchResult, HistorySearchError> {
    validate(character, view, query, config)?;
    let messages = scoped_candidates(view, config.max_content_chars, window)?;
    if messages.len() > 1024 {
        return Err(HistorySearchError::CorpusLimit {
            found: messages.len(),
            maximum: 1024,
        });
    }
    #[cfg(not(feature = "sqlite-system"))]
    {
        let _ = messages;
        Err(HistorySearchError::FtsUnavailable)
    }
    #[cfg(feature = "sqlite-system")]
    {
        search_history_fts_sqlite(character, view, query, config, &messages)
    }
}

#[cfg(feature = "sqlite-system")]
fn search_history_fts_sqlite(
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
    messages: &[ProjectedMessage],
) -> Result<HistorySearchResult, HistorySearchError> {
    use rusqlite::{Connection, params};

    let mut connection = Connection::open_in_memory()
        .map_err(|_| HistorySearchError::FtsFailure("cannot open derived index"))?;
    connection
        .execute_batch(
            "CREATE VIRTUAL TABLE scoped_history_fts USING fts5(content, tokenize='unicode61');",
        )
        .map_err(|_| HistorySearchError::FtsUnavailable)?;
    {
        let transaction = connection
            .transaction()
            .map_err(|_| HistorySearchError::FtsFailure("cannot start derived indexing"))?;
        {
            let mut insert = transaction
                .prepare("INSERT INTO scoped_history_fts(rowid, content) VALUES (?1, ?2)")
                .map_err(|_| HistorySearchError::FtsFailure("cannot prepare derived indexing"))?;
            for (index, message) in messages.iter().enumerate() {
                insert
                    .execute(params![index as i64 + 1, &message.content])
                    .map_err(|_| HistorySearchError::FtsFailure("cannot build derived index"))?;
            }
        }
        transaction
            .commit()
            .map_err(|_| HistorySearchError::FtsFailure("cannot finalize derived index"))?;
    }
    let match_query = tokens(query)
        .into_iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut statement = connection
        .prepare("SELECT rowid, bm25(scoped_history_fts) AS rank FROM scoped_history_fts WHERE scoped_history_fts MATCH ?1 ORDER BY rank, rowid LIMIT 1024")
        .map_err(|_| HistorySearchError::FtsFailure("cannot prepare FTS query"))?;
    let rows = statement
        .query_map([match_query], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(|_| HistorySearchError::FtsFailure("FTS query failed"))?;
    let mut hits = Vec::new();
    for result in rows {
        let (row_id, bm25) =
            result.map_err(|_| HistorySearchError::FtsFailure("invalid FTS row"))?;
        let Some(index) = row_id
            .checked_sub(1)
            .and_then(|id| usize::try_from(id).ok())
        else {
            return Err(HistorySearchError::FtsFailure(
                "invalid derived source identity",
            ));
        };
        let Some(message) = messages.get(index) else {
            return Err(HistorySearchError::FtsFailure(
                "out-of-range derived source identity",
            ));
        };
        if !bm25.is_finite() {
            return Err(HistorySearchError::FtsFailure("invalid FTS rank"));
        }
        hits.push(hit(character, view, message, bm25));
    }
    let ranked = fuse(
        character,
        &[RetrievalBucket {
            method: RetrievalMethod::Fts,
            hits,
        }],
        FusionConfig {
            mode: FusionMode::Union,
            limit: config.limit,
            minimum_graph: 0,
        },
    )?;
    Ok(finish(view, messages.is_empty(), ranked, None, None))
}

/// Opt-in semantic and lexical/vector-fusion selection over the very same visible
/// Timeline projection. Embeddings reuse the existing semantic-memory provider and
/// derived, model-space-scoped cache, not a parallel history index. Absent index
/// entries are computed; provider failure is returned, never downgraded to lexical.
pub async fn search_history_semantic(
    retriever: &SemanticMemoryRetriever,
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
    mode: FusionMode,
    minimum_cosine: Option<f64>,
) -> Result<HistorySearchResult, HistorySearchError> {
    search_history_semantic_with_window(
        retriever,
        character,
        view,
        query,
        config,
        mode,
        minimum_cosine,
        None,
    )
    .await
}

pub async fn search_archived_history_semantic(
    retriever: &SemanticMemoryRetriever,
    character: CharacterId,
    view: &MitaView,
    window: &HistoryWindow,
    query: &str,
    config: HistorySearchConfig,
    mode: FusionMode,
    minimum_cosine: Option<f64>,
) -> Result<HistorySearchResult, HistorySearchError> {
    search_history_semantic_with_window(
        retriever,
        character,
        view,
        query,
        config,
        mode,
        minimum_cosine,
        Some(window),
    )
    .await
}

async fn search_history_semantic_with_window(
    retriever: &SemanticMemoryRetriever,
    character: CharacterId,
    view: &MitaView,
    query: &str,
    config: HistorySearchConfig,
    mode: FusionMode,
    minimum_cosine: Option<f64>,
    window: Option<&HistoryWindow>,
) -> Result<HistorySearchResult, HistorySearchError> {
    validate(character, view, query, config)?;
    if minimum_cosine.is_some_and(|value| !value.is_finite() || !(-1.0..=1.0).contains(&value)) {
        return Err(HistorySearchError::Configuration(
            "invalid cosine threshold",
        ));
    }
    let fusion = FusionConfig {
        mode,
        limit: config.limit.max(retriever.rerank_candidate_limit()),
        minimum_graph: 0,
    };
    fuse(character, &[], fusion)?;
    let messages = scoped_candidates(view, config.max_content_chars, window)?;
    if messages.len() > 1024 {
        return Err(HistorySearchError::CorpusLimit {
            found: messages.len(),
            maximum: 1024,
        });
    }
    let (space, similarities, cost) = retriever
        .score_history_messages(character, &messages, query)
        .await?;
    let mut ranked = messages.iter().zip(similarities).collect::<Vec<_>>();
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| right.learned_at.cmp(&left.learned_at))
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    let vectors = ranked
        .into_iter()
        .filter(|(_, score)| minimum_cosine.is_none_or(|minimum| *score >= minimum))
        .map(|(message, score)| hit(character, view, message, score))
        .collect();
    let mut buckets = vec![RetrievalBucket {
        method: RetrievalMethod::Vector,
        hits: vectors,
    }];
    if !matches!(mode, FusionMode::VectorOnly) {
        buckets.push(lexical_bucket(character, view, &messages, query));
    }
    let mut hits = fuse(character, &buckets, fusion)?;
    retriever.rerank_fused(query, &mut hits).await?;
    hits.truncate(config.limit);
    Ok(finish(
        view,
        messages.is_empty(),
        hits,
        Some(space),
        Some(cost),
    ))
}
