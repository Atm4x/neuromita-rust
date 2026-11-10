use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use super::{
    embedding::{Embedding, EmbeddingError, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace},
    memory_context::{
        lexical_ranked_scoped, priority_rank, render_ranked_memory_context_scoped, tokens,
    },
    reranking::{RerankPolicy, RerankProvider, rerank_prefix},
    retrieval_fusion::{
        FusionConfig, FusionMode, RetrievalBucket, RetrievalHit, RetrievalKey, RetrievalMethod,
        RetrievalSource, fuse,
    },
};
use crate::{
    identity::CharacterId,
    identity::EventId,
    memory::{CharacterMemories, MemoryKind, MemoryState},
    timeline::domain::{MemoryId, MemoryRecord, ProjectedMessage},
    timeline::selection::TimelineTurn,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    character: CharacterId,
    space: EmbeddingSpace,
    id: CachedSourceId,
    content: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CachedSourceId {
    Memory(MemoryId),
    History(EventId),
}

struct EmbeddingCache {
    entries: HashMap<CacheKey, Embedding>,
    order: VecDeque<CacheKey>,
    revision: u64,
    snapshots: HashMap<CharacterId, AuthoritativeSnapshot>,
    revisions: HashMap<CharacterId, u64>,
    clear_revision: u64,
}

struct AuthoritativeSnapshot {
    turn: TimelineTurn,
    active_ids: HashSet<CachedSourceId>,
}

#[derive(Clone, Copy)]
struct RetrievalBarrier {
    revision: u64,
    clear_revision: u64,
}

impl EmbeddingCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            revision: 0,
            snapshots: HashMap::new(),
            revisions: HashMap::new(),
            clear_revision: 0,
        }
    }
    fn purge_character(&mut self, character: CharacterId, keep: Option<&HashSet<CacheKey>>) {
        self.entries.retain(|key, _| {
            key.character != character || keep.is_some_and(|allowed| allowed.contains(key))
        });
        self.order.retain(|key| self.entries.contains_key(key));
    }

    fn track(
        &mut self,
        character: CharacterId,
        turn: Option<TimelineTurn>,
        keys: &[CacheKey],
    ) -> Result<RetrievalBarrier, EmbeddingError> {
        if let Some(turn) = turn {
            if turn.character != character {
                return Err(EmbeddingError::StaleKnowledge);
            }
            let active = keys.iter().cloned().collect::<HashSet<_>>();
            let active_ids = keys
                .iter()
                .filter(|key| matches!(key.id, CachedSourceId::Memory(_)))
                .map(|key| key.id.clone())
                .collect::<HashSet<_>>();
            let previous = self.snapshots.get(&character);
            if let Some(previous) = previous {
                let newer_selection =
                    turn.selection.generation > previous.turn.selection.generation;
                let same_selection = turn.selection == previous.turn.selection;
                if !newer_selection
                    && (!same_selection
                        || turn.context_head < previous.turn.context_head
                        || (turn.context_head == previous.turn.context_head
                            && turn.context_epoch != previous.turn.context_epoch)
                        || (turn.context_head == previous.turn.context_head
                            && !active_ids.is_subset(&previous.active_ids)))
                {
                    return Err(EmbeddingError::StaleKnowledge);
                }
            }
            let identical = previous
                .is_some_and(|previous| previous.turn == turn && previous.active_ids == active_ids)
                && self
                    .entries
                    .keys()
                    .filter(|key| key.character == character)
                    .all(|key| active.contains(key));
            if !identical {
                let purge_all = previous.is_some_and(|previous| {
                    previous.turn.selection != turn.selection
                        || previous.turn.context_epoch != turn.context_epoch
                });
                self.purge_character(character, (!purge_all).then_some(&active));
                self.revision = self.revision.wrapping_add(1);
                *self.revisions.entry(character).or_default() += 1;
                self.snapshots
                    .insert(character, AuthoritativeSnapshot { turn, active_ids });
            }
        }
        Ok(RetrievalBarrier {
            revision: *self.revisions.get(&character).unwrap_or(&0),
            clear_revision: self.clear_revision,
        })
    }

    fn unchanged(&self, character: CharacterId, barrier: RetrievalBarrier) -> bool {
        self.clear_revision == barrier.clear_revision
            && *self.revisions.get(&character).unwrap_or(&0) == barrier.revision
    }

    fn insert(&mut self, key: CacheKey, embedding: Embedding, max: usize) {
        if self.entries.contains_key(&key) {
            return;
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, embedding);
        self.revision = self.revision.wrapping_add(1);
        while self.order.len() > max {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddingCachePreview {
    pub space: EmbeddingSpace,
    pub total_entries: usize,
    pub entries_by_character: BTreeMap<CharacterId, usize>,
    instance: uuid::Uuid,
    revision: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EmbeddingCacheMaintenanceError {
    #[error("embedding cache clearing requires explicit confirmation")]
    ConfirmationRequired,
    #[error("embedding cache preview is stale or belongs to another retriever")]
    StalePreview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalCost {
    pub embedding_requests: usize,
    pub embedded_texts: usize,
    pub embedded_characters: usize,
    pub cache_hits: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SemanticRetrievalFilter {
    pub minimum_cosine: Option<f64>,
}

impl SemanticRetrievalFilter {
    pub const fn all() -> Self {
        Self {
            minimum_cosine: None,
        }
    }

    fn validate(self) -> Result<Self, EmbeddingError> {
        if self
            .minimum_cosine
            .is_some_and(|score| !score.is_finite() || !(-1.0..=1.0).contains(&score))
        {
            return Err(EmbeddingError::Configuration(
                "minimum cosine must be finite and within [-1, 1]".into(),
            ));
        }
        Ok(self)
    }
}

/// Explicit visibility policy for a read from one captured Timeline memory projection.
/// Default retrieval remains active-only; forgotten facts are never admitted implicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRetrievalScope {
    ActiveOnly,
    ActiveAndForgotten,
    ForgottenOnly,
}

impl MemoryRetrievalScope {
    fn admits(self, record: &MemoryRecord, as_of_unix_ms: u64) -> bool {
        match (self, record.state) {
            (Self::ActiveOnly | Self::ActiveAndForgotten, MemoryState::Active) => {
                record.available_at(as_of_unix_ms)
            }
            (Self::ActiveAndForgotten | Self::ForgottenOnly, MemoryState::Forgotten) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalStatus {
    EmptyCorpus,
    NoMatches,
    Selected,
}

/// One admitted Timeline memory. The ID identifies the source; the embedding
/// space in RetrievedMemory identifies how its similarity was evaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievedMemoryMatch {
    pub id: MemoryId,
    pub cosine_similarity: f64,
    pub fusion_score: f64,
    pub methods: Vec<RetrievalMethod>,
}

#[derive(Debug)]
pub struct RetrievedMemory {
    pub rendered: Option<String>,
    pub selected_ids: Vec<MemoryId>,
    pub matches: Vec<RetrievedMemoryMatch>,
    pub space: EmbeddingSpace,
    pub status: RetrievalStatus,
    pub cost: RetrievalCost,
}

/// A bounded, derived embedding cache. Membership always comes from the captured Timeline projection.
/// No network await is performed while holding the cache lock.
pub struct SemanticMemoryRetriever {
    provider: Arc<dyn EmbeddingProvider>,
    cache: Mutex<EmbeddingCache>,
    instance: uuid::Uuid,
    max_entries: usize,
    reranker: Option<(Arc<dyn RerankProvider>, RerankPolicy)>,
}

impl SemanticMemoryRetriever {
    pub fn new(
        provider: Arc<dyn EmbeddingProvider>,
        max_entries: usize,
    ) -> Result<Self, EmbeddingError> {
        if max_entries == 0 {
            return Err(EmbeddingError::Configuration(
                "cache capacity must be positive".into(),
            ));
        }
        Ok(Self {
            provider,
            cache: Mutex::new(EmbeddingCache::new()),
            instance: uuid::Uuid::new_v4(),
            max_entries,
            reranker: None,
        })
    }

    pub fn with_reranker(
        mut self,
        provider: Arc<dyn RerankProvider>,
        policy: RerankPolicy,
    ) -> Result<Self, EmbeddingError> {
        self.reranker = Some((provider, policy.validate()?));
        Ok(self)
    }

    /// The number of fused History candidates that a configured reranker can
    /// reconsider before the user's final result limit is applied.
    pub(crate) fn rerank_candidate_limit(&self) -> usize {
        self.reranker.as_ref().map_or(0, |(_, policy)| policy.limit)
    }

    /// Apply the single configured reranker to any already-validated, character-scoped
    /// fusion output. Memory and History share the same provider and strict error policy.
    pub(crate) async fn rerank_fused(
        &self,
        query: &str,
        hits: &mut [super::retrieval_fusion::FusedHit],
    ) -> Result<(), EmbeddingError> {
        if let Some((provider, policy)) = &self.reranker {
            rerank_prefix(provider.as_ref(), query, hits, *policy).await?;
        }
        Ok(())
    }

    /// Read-only, model-space scoped inspection of the derived in-memory index.
    /// Never materializes memory text, vectors, or provider secrets.
    pub fn preview_cache_clear(&self) -> EmbeddingCachePreview {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut entries_by_character = BTreeMap::new();
        for key in cache.entries.keys() {
            *entries_by_character.entry(key.character).or_insert(0) += 1;
        }
        EmbeddingCachePreview {
            space: self.provider.space().clone(),
            total_entries: cache.entries.len(),
            entries_by_character,
            instance: self.instance,
            revision: cache.revision,
        }
    }

    /// The user-facing maintenance boundary. Rejects stale or foreign previews.
    /// In-flight provider results may finish for their original caller, but cannot
    /// repopulate this cache if they began before the confirmed clear.
    pub fn clear_cache_confirmed(
        &self,
        preview: &EmbeddingCachePreview,
        confirmed: bool,
    ) -> Result<usize, EmbeddingCacheMaintenanceError> {
        if !confirmed {
            return Err(EmbeddingCacheMaintenanceError::ConfirmationRequired);
        }
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if preview.instance != self.instance || preview.revision != cache.revision {
            return Err(EmbeddingCacheMaintenanceError::StalePreview);
        }
        Ok(Self::clear_locked_cache(&mut cache))
    }

    pub(crate) fn needs_authoritative_refresh(
        &self,
        character: CharacterId,
        turn: TimelineTurn,
    ) -> bool {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        match cache.snapshots.get(&character) {
            Some(snapshot) => snapshot.turn != turn,
            None => cache.entries.keys().any(|key| key.character == character),
        }
    }

    /// Adopt a separately captured Timeline projection without starting any
    /// embedding request. The same revision barrier governs retrieval and
    /// idle reconciliation, so old in-flight work cannot restore evicted data.
    pub(crate) fn refresh_authoritative(
        &self,
        character: CharacterId,
        turn: TimelineTurn,
        memories: &[MemoryRecord],
    ) -> Result<(), EmbeddingError> {
        let space = self.provider.space().clone();
        let keys = memories
            .iter()
            .filter(|record| {
                matches!(record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
                    && record.state == MemoryState::Active
            })
            .map(|record| CacheKey {
                character,
                space: space.clone(),
                id: CachedSourceId::Memory(record.id),
                content: record.content.clone(),
            })
            .collect::<Vec<_>>();
        self.cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .track(character, Some(turn), &keys)?;
        Ok(())
    }

    pub fn clear_cache(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Self::clear_locked_cache(&mut cache);
    }

    fn clear_locked_cache(cache: &mut EmbeddingCache) -> usize {
        let cleared = cache.entries.len();
        cache.entries.clear();
        cache.order.clear();
        cache.revision = cache.revision.wrapping_add(1);
        cache.clear_revision = cache.clear_revision.wrapping_add(1);
        cleared
    }

    pub async fn retrieve(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        self.retrieve_filtered(character, memories, query, SemanticRetrievalFilter::all())
            .await
    }

    /// Explicitly filter cosine similarity without changing the captured source
    /// membership, the embedding cache or the provider error policy.
    pub async fn retrieve_filtered(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
        filter: SemanticRetrievalFilter,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        self.retrieve_with_fusion(character, memories, query, filter, FusionMode::VectorOnly)
            .await
    }

    /// Explicit source fusion over the same captured memory projection. There is
    /// no second index or provider; the lexical bucket is computed locally.
    pub async fn retrieve_with_fusion(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
        filter: SemanticRetrievalFilter,
        mode: FusionMode,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        self.retrieve_with_fusion_at(character, memories, query, filter, mode, None)
            .await
    }

    pub(crate) async fn retrieve_with_fusion_at(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
        filter: SemanticRetrievalFilter,
        mode: FusionMode,
        turn: Option<TimelineTurn>,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        let admitted = memories
            .iter()
            .filter(|record| {
                matches!(record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
                    && record.state == MemoryState::Active
            })
            .cloned()
            .collect();
        self.retrieve_admitted_with_fusion(character, admitted, query, filter, mode, false, turn)
            .await
    }

    /// Opt-in read of a single character's captured authoritative memory projection.
    /// Active memories must be available at `as_of_unix_ms`; forgotten entries are
    /// retained historical facts and require an explicitly selected scope. Deleted
    /// entries are never searchable, and this method never restores any memory.
    /// Callers are responsible for authorizing historical recall before rendering
    /// its returned context into a model prompt.
    pub async fn retrieve_scoped(
        &self,
        character: CharacterId,
        snapshot: &CharacterMemories,
        query: &str,
        as_of_unix_ms: u64,
        scope: MemoryRetrievalScope,
        filter: SemanticRetrievalFilter,
        mode: FusionMode,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        let admitted = snapshot
            .all()
            .into_iter()
            .filter(|record| {
                matches!(record.kind, MemoryKind::Fact | MemoryKind::RagOnly)
                    && scope.admits(record, as_of_unix_ms)
            })
            .collect();
        self.retrieve_admitted_with_fusion(
            character,
            admitted,
            query,
            filter,
            mode,
            scope != MemoryRetrievalScope::ActiveOnly,
            None,
        )
        .await
    }

    /// Rank only facts that were already admitted by an explicit, validated
    /// forgotten-memory recall. Never broaden the selected IDs to the corpus.
    pub(crate) async fn retrieve_recalled_forgotten(
        &self,
        character: CharacterId,
        records: Vec<MemoryRecord>,
        query: &str,
        mode: FusionMode,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        self.retrieve_admitted_with_fusion(
            character,
            records,
            query,
            SemanticRetrievalFilter::all(),
            mode,
            true,
            None,
        )
        .await
    }

    /// The same derived cache and provider serves both canonical memory and
    /// currently visible Timeline history. History membership is never cached.
    pub(crate) async fn score_history_messages(
        &self,
        character: CharacterId,
        messages: &[ProjectedMessage],
        query: &str,
    ) -> Result<(EmbeddingSpace, Vec<f64>, RetrievalCost), EmbeddingError> {
        let sources = messages
            .iter()
            .map(|message| {
                (
                    CachedSourceId::History(message.event_id),
                    message.content.as_str(),
                )
            })
            .collect::<Vec<_>>();
        self.score_canonical_documents(character, &sources, query, None)
            .await
    }

    /// Score one caller-admitted Memory+History corpus using one query embedding.
    /// Source identity remains typed in the existing derived embedding cache.
    pub(crate) async fn score_joint_generation(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        messages: &[ProjectedMessage],
        query: &str,
        turn: TimelineTurn,
    ) -> Result<(EmbeddingSpace, Vec<f64>, Vec<f64>, RetrievalCost), EmbeddingError> {
        let sources = memories
            .iter()
            .map(|record| (CachedSourceId::Memory(record.id), record.content.as_str()))
            .chain(messages.iter().map(|message| {
                (
                    CachedSourceId::History(message.event_id),
                    message.content.as_str(),
                )
            }))
            .collect::<Vec<_>>();
        let (space, similarities, cost) = self
            .score_canonical_documents(character, &sources, query, Some(turn))
            .await?;
        let (memory, history) = similarities.split_at(memories.len());
        Ok((space, memory.to_vec(), history.to_vec(), cost))
    }

    /// Only source identity + content + immutable model-space enter this cache.
    /// No Timeline membership, branch visibility, or historical access survives
    /// between calls; each caller must supply its freshly captured projection.
    async fn score_canonical_documents(
        &self,
        character: CharacterId,
        sources: &[(CachedSourceId, &str)],
        query: &str,
        turn: Option<TimelineTurn>,
    ) -> Result<(EmbeddingSpace, Vec<f64>, RetrievalCost), EmbeddingError> {
        let space = self.provider.space().clone();
        let mut cost = RetrievalCost {
            embedding_requests: 0,
            embedded_texts: 0,
            embedded_characters: 0,
            cache_hits: 0,
        };

        if sources.len() > 1024 {
            return Err(EmbeddingError::Configuration(
                "semantic source exceeds bounded per-query capacity".into(),
            ));
        }
        let query = query.chars().take(2048).collect::<String>();
        if query.trim().is_empty() {
            return Err(EmbeddingError::Configuration(
                "semantic query is empty".into(),
            ));
        }
        let keys = sources
            .iter()
            .map(|(id, content)| CacheKey {
                character,
                space: space.clone(),
                id: id.clone(),
                content: (*content).to_string(),
            })
            .collect::<Vec<_>>();
        let barrier = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .track(character, turn, &keys)?;
        if sources.is_empty() {
            return Ok((space, Vec::new(), cost));
        }
        let mut vectors: Vec<Option<Embedding>> = vec![None; sources.len()];
        let mut missing = Vec::new();

        {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !cache.unchanged(character, barrier) {
                return Err(EmbeddingError::StaleKnowledge);
            }
            for (index, key) in keys.iter().enumerate() {
                if let Some(vector) = cache.entries.get(key) {
                    cost.cache_hits += 1;
                    vectors[index] = Some(vector.clone());
                } else {
                    missing.push(index);
                }
            }
        }
        let query_vector = self
            .provider
            .embed(EmbeddingPurpose::Query, query.clone())
            .await?;
        cost.embedded_characters += query.chars().count();
        cost.embedding_requests += 1;
        cost.embedded_texts += 1;
        query_vector.ensure_space(&space)?;
        if query_vector.values.len() != space.dimensions
            || query_vector.values.iter().any(|value| !value.is_finite())
        {
            return Err(EmbeddingError::Response("invalid query embedding".into()));
        }
        if !missing.is_empty() {
            if !self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .unchanged(character, barrier)
            {
                return Err(EmbeddingError::StaleKnowledge);
            }
            let texts = missing
                .iter()
                .map(|&index| sources[index].1.to_string())
                .collect::<Vec<_>>();
            cost.embedded_characters +=
                texts.iter().map(|text| text.chars().count()).sum::<usize>();
            let fresh = self.provider.embed_documents(texts).await?;
            cost.embedding_requests += self.provider.document_request_count(missing.len());
            cost.embedded_texts += missing.len();
            if fresh.len() != missing.len() {
                return Err(EmbeddingError::Response(
                    "document embedding count mismatch".into(),
                ));
            }
            for vector in &fresh {
                vector.ensure_space(&space)?;
            }
            if fresh.iter().any(|vector| {
                vector.values.len() != space.dimensions
                    || vector.values.iter().any(|value| !value.is_finite())
            }) {
                return Err(EmbeddingError::Response(
                    "invalid document embedding".into(),
                ));
            }
            if self.provider.space() != &space {
                return Err(EmbeddingError::Configuration(
                    "embedding space changed during retrieval".into(),
                ));
            }
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for (&index, vector) in missing.iter().zip(fresh) {
                vectors[index] = Some(vector.clone());
                if !cache.unchanged(character, barrier) {
                    return Err(EmbeddingError::StaleKnowledge);
                }
                if turn.is_some() || !cache.snapshots.contains_key(&character) {
                    cache.insert(keys[index].clone(), vector, self.max_entries);
                }
            }
        }
        if self.provider.space() != &space {
            return Err(EmbeddingError::Configuration(
                "embedding space changed during retrieval".into(),
            ));
        }
        Ok((
            space,
            vectors
                .iter()
                .map(|vector| {
                    vector
                        .as_ref()
                        .expect("filled embedding")
                        .similarity(&query_vector)
                })
                .collect(),
            cost,
        ))
    }

    async fn retrieve_admitted_with_fusion(
        &self,
        character: CharacterId,
        memories: Vec<MemoryRecord>,
        query: &str,
        filter: SemanticRetrievalFilter,
        mode: FusionMode,
        include_forgotten: bool,
        turn: Option<TimelineTurn>,
    ) -> Result<RetrievedMemory, EmbeddingError> {
        let filter = filter.validate()?;
        let config = FusionConfig {
            mode,
            limit: 1024,
            minimum_graph: 0,
        };
        fuse(character, &[], config).map_err(|e| EmbeddingError::Configuration(e.to_string()))?;
        if memories.is_empty() {
            self.cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .track(character, turn, &[])?;
            let space = self.provider.space().clone();
            let cost = RetrievalCost {
                embedding_requests: 0,
                embedded_texts: 0,
                embedded_characters: 0,
                cache_hits: 0,
            };
            return Ok(RetrievedMemory {
                rendered: None,
                selected_ids: Vec::new(),
                matches: Vec::new(),
                space,
                status: RetrievalStatus::EmptyCorpus,
                cost,
            });
        }
        let sources = memories
            .iter()
            .map(|record| (CachedSourceId::Memory(record.id), record.content.as_str()))
            .collect::<Vec<_>>();
        let (space, similarities, cost) = self
            .score_canonical_documents(character, &sources, &query, turn)
            .await?;
        let mut ranked = memories.iter().zip(similarities).collect::<Vec<_>>();
        ranked.sort_by(|(a, ascore), (b, bscore)| {
            bscore
                .total_cmp(ascore)
                .then_with(|| priority_rank(b.priority).cmp(&priority_rank(a.priority)))
                .then_with(|| a.id.cmp(&b.id))
        });
        let scores = ranked
            .iter()
            .map(|(memory, score)| (memory.id, *score))
            .collect::<HashMap<_, _>>();
        let vector_hits = ranked
            .iter()
            .filter(|(_, score)| filter.minimum_cosine.is_none_or(|min| *score >= min))
            .map(|(memory, score)| RetrievalHit {
                key: RetrievalKey {
                    character,
                    source: RetrievalSource::Memory,
                    id: memory.id.0.to_string(),
                },
                content: memory.content.clone(),
                raw_score: Some(*score),
                metadata: BTreeMap::new(),
            })
            .collect::<Vec<_>>();
        let mut buckets = vec![RetrievalBucket {
            method: RetrievalMethod::Vector,
            hits: vector_hits,
        }];
        if !matches!(mode, FusionMode::VectorOnly) {
            let query_tokens = tokens(&query);
            let lexical_hits = lexical_ranked_scoped(&memories, &query, include_forgotten)
                .into_iter()
                .filter(|memory| {
                    tokens(&memory.content)
                        .intersection(&query_tokens)
                        .next()
                        .is_some()
                })
                .map(|memory| RetrievalHit {
                    key: RetrievalKey {
                        character,
                        source: RetrievalSource::Memory,
                        id: memory.id.0.to_string(),
                    },
                    content: memory.content.clone(),
                    raw_score: None,
                    metadata: BTreeMap::new(),
                })
                .collect();
            buckets.push(RetrievalBucket {
                method: RetrievalMethod::Keyword,
                hits: lexical_hits,
            });
        }
        let mut fused = fuse(character, &buckets, config)
            .map_err(|error| EmbeddingError::Response(error.to_string()))?;
        self.rerank_fused(query, &mut fused).await?;
        let by_id = memories
            .iter()
            .map(|memory| (memory.id.0.to_string(), memory))
            .collect::<HashMap<_, _>>();
        let (rendered, selected_ids) = render_ranked_memory_context_scoped(
            fused
                .iter()
                .filter_map(|hit| by_id.get(&hit.key.id).copied()),
            include_forgotten,
        );
        let selected_set = selected_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let matches = fused
            .into_iter()
            .filter_map(|hit| {
                let memory = by_id.get(&hit.key.id)?;
                selected_set
                    .contains(&memory.id)
                    .then(|| RetrievedMemoryMatch {
                        id: memory.id,
                        cosine_similarity: scores[&memory.id],
                        fusion_score: hit.score,
                        methods: hit.evidence.into_iter().map(|e| e.method).collect(),
                    })
            })
            .collect();
        if !self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .unchanged(character, barrier)
        {
            return Err(EmbeddingError::StaleKnowledge);
        }
        let status = if selected_ids.is_empty() {
            RetrievalStatus::NoMatches
        } else {
            RetrievalStatus::Selected
        };
        Ok(RetrievedMemory {
            rendered,
            selected_ids,
            matches,
            space,
            status,
            cost,
        })
    }
}

#[cfg(test)]
mod b84_tests {
    use super::*;
    use crate::timeline::domain::{MemoryId, MemoryPriority};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountedEmbeddingProvider {
        space: EmbeddingSpace,
        calls: AtomicUsize,
    }

    impl EmbeddingProvider for CountedEmbeddingProvider {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(
            &self,
            _: EmbeddingPurpose,
            _: String,
        ) -> super::super::embedding::EmbeddingFuture<'_, Embedding> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Embedding::new(vec![1.0, 0.5], &self.space)
            })
        }
    }

    #[tokio::test]
    async fn b84_snapshot_membership_is_not_extended_by_resident_embeddings() {
        let provider = Arc::new(CountedEmbeddingProvider {
            space: EmbeddingSpace::new("test", "b84", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retriever = SemanticMemoryRetriever::new(provider.clone(), 8).unwrap();
        let mut memory = MemoryRecord {
            entities: vec![],
            id: MemoryId(uuid::Uuid::new_v4()),
            priority: MemoryPriority::Normal,
            content: "private previous memory".into(),
            kind: MemoryKind::Fact,
            state: MemoryState::Active,
            protected: false,
            expires_at_unix_ms: None,
        };
        let previous = retriever
            .retrieve(CharacterId::Crazy, &[memory.clone()], "private")
            .await
            .unwrap();
        assert_eq!(previous.selected_ids, vec![memory.id]);
        assert!(
            previous
                .rendered
                .unwrap()
                .contains("private previous memory")
        );
        assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
        assert_eq!(retriever.cache.lock().unwrap().entries.len(), 1);

        let invisible = retriever
            .retrieve(CharacterId::Crazy, &[], "private")
            .await
            .unwrap();
        assert!(invisible.selected_ids.is_empty());
        assert!(invisible.rendered.is_none());
        assert_eq!(provider.calls.load(Ordering::Relaxed), 2);

        memory.state = MemoryState::Forgotten;
        let forgotten = retriever
            .retrieve(CharacterId::Crazy, &[memory.clone()], "private")
            .await
            .unwrap();
        assert!(forgotten.selected_ids.is_empty());
        assert_eq!(provider.calls.load(Ordering::Relaxed), 2);
        assert_eq!(retriever.cache.lock().unwrap().entries.len(), 1);

        memory.state = MemoryState::Active;
        memory.content = "newly revealed replacement".into();
        let replacement = retriever
            .retrieve(CharacterId::Crazy, &[memory.clone()], "revealed")
            .await
            .unwrap();
        assert_eq!(replacement.selected_ids, vec![memory.id]);
        assert!(
            replacement
                .rendered
                .unwrap()
                .contains("newly revealed replacement")
        );
        assert_eq!(replacement.cost.cache_hits, 0);
        assert_eq!(provider.calls.load(Ordering::Relaxed), 4);
        assert_eq!(retriever.cache.lock().unwrap().entries.len(), 2);

        retriever.clear_cache();
        assert!(retriever.cache.lock().unwrap().entries.is_empty());
        let again = retriever
            .retrieve(CharacterId::Crazy, &[memory], "revealed")
            .await
            .unwrap();
        assert_eq!(again.cost.cache_hits, 0);
    }
}

#[cfg(test)]
mod b85_tests;

#[cfg(test)]
mod b86_tests;
