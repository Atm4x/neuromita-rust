//! Explicit fusion of bounded semantic memory results and Timeline-derived graph evidence.
//! Neither source is owned by this adapter; the caller supplies one character's captured projection.

use std::collections::BTreeMap;

use crate::{identity::CharacterId, memory::CharacterMemories};

use super::{
    embedding::{EmbeddingError, EmbeddingSpace},
    graph::{GraphError, query_extracted_graph},
    retrieval_fusion::{
        FusedHit, FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit,
        RetrievalKey, RetrievalMethod, RetrievalSource, fuse,
    },
    semantic_memory::{
        RetrievalCost, RetrievalStatus, SemanticMemoryRetriever, SemanticRetrievalFilter,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum GraphRetrievalError {
    #[error("invalid graph retrieval query: {0:?}")]
    Graph(GraphError),
    #[error("semantic retrieval failed: {0}")]
    Embedding(#[from] EmbeddingError),
    #[error("retrieval fusion failed: {0}")]
    Fusion(#[from] FusionError),
    #[error("semantic results do not belong to the supplied memory projection")]
    InvalidMemoryResult,
}

#[derive(Debug)]
pub struct GraphRetrievalResult {
    pub hits: Vec<FusedHit>,
    pub memory_status: RetrievalStatus,
    pub embedding_space: EmbeddingSpace,
    pub cost: RetrievalCost,
}

/// An explicit opt-in, non-mutating read of both semantic facts and extracted graph relations.
/// The caller provides the authoritative character-scoped projection and as-of timestamp.
/// A failing semantic provider is an error, never silently downgraded to graph-only retrieval.
/// Graph candidates are lexical, source-versioned, bounded to 64 and never embedded.
/// Semantic candidates are the existing bounded memory shortlist, not a new vector index.
pub async fn retrieve_memory_with_graph(
    retriever: &SemanticMemoryRetriever,
    character: CharacterId,
    snapshot: &CharacterMemories,
    query: &str,
    as_of_unix_ms: u64,
    config: FusionConfig,
) -> Result<GraphRetrievalResult, GraphRetrievalError> {
    fuse(character, &[], config)?;
    let graph = query_extracted_graph(snapshot, query, as_of_unix_ms, config.limit.min(64))
        .map_err(GraphRetrievalError::Graph)?;
    let memories = snapshot.searchable_at(as_of_unix_ms);
    let retrieved = retriever
        .retrieve_with_fusion(
            character,
            &memories,
            query,
            SemanticRetrievalFilter::all(),
            FusionMode::VectorOnly,
        )
        .await?;

    let by_id = memories
        .iter()
        .map(|memory| (memory.id, memory))
        .collect::<BTreeMap<_, _>>();
    let mut vector_hits = Vec::with_capacity(retrieved.matches.len());
    for matched in &retrieved.matches {
        let record = by_id
            .get(&matched.id)
            .ok_or(GraphRetrievalError::InvalidMemoryResult)?;
        vector_hits.push(RetrievalHit {
            key: RetrievalKey {
                character,
                source: RetrievalSource::Memory,
                id: record.id.0.to_string(),
            },
            content: record.content.clone(),
            raw_score: Some(matched.cosine_similarity),
            metadata: BTreeMap::new(),
        });
    }

    let graph_hits = graph
        .into_iter()
        .map(|hit| {
            let relation = &hit.provenance.relation;
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "source_memory_id".into(),
                hit.provenance.source.0.to_string(),
            );
            metadata.insert(
                "source_revision".into(),
                hit.provenance.source_revision.to_string(),
            );
            let id = format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                hit.provenance.source.0,
                hit.provenance.source_revision,
                relation.subject.len(),
                relation.subject,
                relation.predicate.len(),
                relation.predicate,
                relation.object.len(),
                relation.object,
            );
            RetrievalHit {
                key: RetrievalKey {
                    character,
                    source: RetrievalSource::Graph,
                    id,
                },
                content: format!(
                    "{} | {} | {}",
                    relation.subject, relation.predicate, relation.object
                ),
                raw_score: Some(hit.matching_terms as f64),
                metadata,
            }
        })
        .collect();
    let buckets = [
        RetrievalBucket {
            method: RetrievalMethod::Vector,
            hits: vector_hits,
        },
        RetrievalBucket {
            method: RetrievalMethod::Graph,
            hits: graph_hits,
        },
    ];
    Ok(GraphRetrievalResult {
        hits: fuse(character, &buckets, config)?,
        memory_status: retrieved.status,
        embedding_space: retrieved.space,
        cost: retrieved.cost,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use crate::{
        knowledge::embedding::{
            Embedding, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace,
        },
        memory::{MemoryChange, MemoryId, MemoryPriority, MemoryRecord, MemoryRelation},
    };

    use super::*;

    struct Provider {
        space: EmbeddingSpace,
        calls: AtomicUsize,
        unavailable: bool,
    }

    impl Provider {
        fn new(unavailable: bool) -> Self {
            Self {
                space: EmbeddingSpace::new("test", "graph", "v1", 2).unwrap(),
                calls: AtomicUsize::new(0),
                unavailable,
            }
        }
    }

    impl EmbeddingProvider for Provider {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(&self, _: EmbeddingPurpose, _: String) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if self.unavailable {
                    return Err(EmbeddingError::Transport("fixture offline".into()));
                }
                Embedding::new(vec![1.0, 0.0], &self.space)
            })
        }
    }

    fn snapshot() -> (CharacterMemories, MemoryId) {
        let mut snapshot = CharacterMemories::default();
        let source = MemoryId::new();
        snapshot
            .apply_committed(&MemoryChange::Upsert(MemoryRecord::fact(
                source,
                MemoryPriority::Normal,
                "Mita prefers tea",
            )))
            .unwrap();
        let revision = snapshot.source_revision(source).unwrap();
        snapshot
            .apply_committed(&MemoryChange::ReplaceExtractedRelations(
                crate::memory::ExtractedRelations {
                    source,
                    source_revision: revision,
                    relations: vec![MemoryRelation {
                        subject: "Mita".into(),
                        predicate: "likes".into(),
                        object: "tea".into(),
                    }],
                },
            ))
            .unwrap();
        (snapshot, source)
    }

    fn config(mode: FusionMode) -> FusionConfig {
        FusionConfig {
            mode,
            limit: 8,
            minimum_graph: 0,
        }
    }

    #[tokio::test]
    async fn mixed_vector_and_graph_evidence_preserves_source_and_revision() {
        let (snapshot, source) = snapshot();
        let provider = Arc::new(Provider::new(false));
        let retriever = SemanticMemoryRetriever::new(provider, 10).unwrap();
        let result = retrieve_memory_with_graph(
            &retriever,
            CharacterId::Crazy,
            &snapshot,
            "tea",
            0,
            config(FusionMode::Union),
        )
        .await
        .unwrap();
        assert_eq!(result.hits.len(), 2);
        let graph = result
            .hits
            .iter()
            .find(|hit| hit.key.source == RetrievalSource::Graph)
            .unwrap();
        assert_eq!(graph.metadata["source_memory_id"], source.0.to_string());
        assert_eq!(
            graph.metadata["source_revision"],
            snapshot.source_revision(source).unwrap().to_string()
        );
        assert_eq!(graph.evidence[0].method, RetrievalMethod::Graph);
        assert_eq!(graph.content, "Mita | likes | tea");
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.key.source == RetrievalSource::Memory)
        );
    }

    #[tokio::test]
    async fn vector_only_excludes_graph_and_provider_failure_is_not_a_fallback() {
        let (snapshot, _) = snapshot();
        let provider = Arc::new(Provider::new(false));
        let retriever = SemanticMemoryRetriever::new(provider, 10).unwrap();
        let selected = retrieve_memory_with_graph(
            &retriever,
            CharacterId::Crazy,
            &snapshot,
            "tea",
            0,
            config(FusionMode::VectorOnly),
        )
        .await
        .unwrap();
        assert_eq!(selected.hits.len(), 1);
        assert_eq!(selected.hits[0].key.source, RetrievalSource::Memory);

        let down = SemanticMemoryRetriever::new(Arc::new(Provider::new(true)), 10).unwrap();
        assert!(matches!(
            retrieve_memory_with_graph(
                &down,
                CharacterId::Crazy,
                &snapshot,
                "tea",
                0,
                config(FusionMode::Union)
            )
            .await,
            Err(GraphRetrievalError::Embedding(EmbeddingError::Transport(_)))
        ));
    }

    #[tokio::test]
    async fn source_expiry_and_revision_change_invalidate_graph_and_memory() {
        let (mut snapshot, source) = snapshot();
        let provider = Arc::new(Provider::new(false));
        let retriever = SemanticMemoryRetriever::new(provider.clone(), 10).unwrap();
        let mut expired = snapshot.get(source).unwrap().clone();
        expired.expires_at_unix_ms = Some(100);
        snapshot
            .apply_committed(&MemoryChange::Upsert(expired))
            .unwrap();
        let after = retrieve_memory_with_graph(
            &retriever,
            CharacterId::Crazy,
            &snapshot,
            "tea",
            100,
            config(FusionMode::Union),
        )
        .await
        .unwrap();
        assert!(after.hits.is_empty());
        assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    }
}
