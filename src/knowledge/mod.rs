//! Deterministic knowledge selection over caller-owned, snapshot-scoped facts.
//! The embedding port is an abstraction; provider implementations live outside this module.

pub mod embedding;
pub mod memory_context;
pub mod retrieval_fusion;
pub mod semantic_memory;

use std::sync::Arc;

use crate::{identity::CharacterId, timeline::domain::MemoryRecord};
use embedding::EmbeddingError;
use memory_context::render_memory_context;
use retrieval_fusion::{FusionConfig, FusionMode, fuse};
use semantic_memory::{SemanticMemoryRetriever, SemanticRetrievalFilter};

/// Explicit selection policy. Knowledge does not own canonical Timeline records or model runtimes.
pub struct KnowledgeRetrieval {
    mode: KnowledgeMode,
}

enum KnowledgeMode {
    Lexical,
    Semantic {
        retriever: Arc<SemanticMemoryRetriever>,
        fusion: FusionMode,
    },
}

impl KnowledgeRetrieval {
    pub fn lexical() -> Self {
        Self {
            mode: KnowledgeMode::Lexical,
        }
    }

    pub fn semantic(retriever: Arc<SemanticMemoryRetriever>) -> Self {
        Self {
            mode: KnowledgeMode::Semantic {
                retriever,
                fusion: FusionMode::VectorOnly,
            },
        }
    }

    pub fn semantic_fusion(
        retriever: Arc<SemanticMemoryRetriever>,
        fusion: FusionMode,
    ) -> Result<Self, EmbeddingError> {
        fuse(
            CharacterId::Mita,
            &[],
            FusionConfig {
                mode: fusion,
                limit: 1024,
                minimum_graph: 0,
            },
        )
        .map_err(|error| EmbeddingError::Configuration(error.to_string()))?;
        Ok(Self {
            mode: KnowledgeMode::Semantic { retriever, fusion },
        })
    }

    /// Returns only bounded prompt data from the caller's immutable Timeline projection.
    /// Errors remain errors; no implicit downgrade from semantic to lexical.
    pub async fn select(
        &self,
        character: CharacterId,
        memories: &[MemoryRecord],
        query: &str,
    ) -> Result<Option<String>, EmbeddingError> {
        match &self.mode {
            KnowledgeMode::Lexical => Ok(render_memory_context(memories, query)),
            KnowledgeMode::Semantic { retriever, fusion } => {
                let result = retriever
                    .retrieve_with_fusion(
                        character,
                        memories,
                        query,
                        SemanticRetrievalFilter::all(),
                        *fusion,
                    )
                    .await?;
                tracing::debug!(
                    character = %character,
                    selected = result.selected_ids.len(),
                    embedding_requests = result.cost.embedding_requests,
                    embedded_texts = result.cost.embedded_texts,
                    embedded_characters = result.cost.embedded_characters,
                    cache_hits = result.cost.cache_hits,
                    "knowledge retrieval finished"
                );
                Ok(result.rendered)
            }
        }
    }
}

impl Default for KnowledgeRetrieval {
    fn default() -> Self {
        Self::lexical()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::domain::{MemoryId, MemoryPriority};
    use embedding::{
        Embedding, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose, EmbeddingSpace,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    struct UnavailableFixture {
        space: EmbeddingSpace,
        calls: AtomicUsize,
    }

    impl EmbeddingProvider for UnavailableFixture {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(&self, _: EmbeddingPurpose, _: String) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::Relaxed);
                Err(EmbeddingError::Transport("fixture not available".into()))
            })
        }
    }

    fn fact() -> MemoryRecord {
        MemoryRecord {
            id: MemoryId(Uuid::from_u128(14)),
            priority: MemoryPriority::Normal,
            content: "ромашковый чай".into(),
        }
    }

    #[tokio::test]
    async fn lexical_selection_requires_no_embedding_provider() {
        let selected = KnowledgeRetrieval::lexical()
            .select(CharacterId::Mita, &[fact()], "чай")
            .await
            .unwrap()
            .unwrap();
        assert!(selected.contains("ромашковый чай"));
        assert!(selected.contains(&Uuid::from_u128(14).to_string()));
    }

    #[tokio::test]
    async fn unavailable_semantic_provider_is_not_silent_lexical_success() {
        let fixture = Arc::new(UnavailableFixture {
            space: EmbeddingSpace::new("fixture", "unavailable", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 10).unwrap());
        let selected = KnowledgeRetrieval::semantic(retriever)
            .select(CharacterId::Mita, &[fact()], "чай")
            .await;
        assert!(matches!(selected, Err(EmbeddingError::Transport(_))));
        assert_eq!(fixture.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn empty_semantic_corpus_does_not_contact_a_model() {
        let fixture = Arc::new(UnavailableFixture {
            space: EmbeddingSpace::new("fixture", "unavailable", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
        });
        let retriever = Arc::new(SemanticMemoryRetriever::new(fixture.clone(), 10).unwrap());
        let selected = KnowledgeRetrieval::semantic(retriever)
            .select(CharacterId::Mita, &[], "чай")
            .await
            .unwrap();
        assert!(selected.is_none());
        assert_eq!(fixture.calls.load(Ordering::Relaxed), 0);
    }
}
