//! Service-owned ports for model-backed semantic operations.
//! The operation contract is independent of the worker implementation.

use std::sync::Arc;

use crate::knowledge::{
    embedding::{
        Embedding, EmbeddingError, EmbeddingFuture, EmbeddingProvider, EmbeddingPurpose,
        EmbeddingSpace,
    },
    reranking::{RerankFuture, RerankProvider},
};

use super::{
    AiService,
    service_workers::{ServiceModel, ServiceWorkerRegistry},
};

fn assert_selected(
    registry: &ServiceWorkerRegistry,
    expected: &ServiceModel,
) -> Result<(), EmbeddingError> {
    if registry
        .active(expected.service)
        .map_err(|error| EmbeddingError::Transport(error.to_string()))?
        .as_ref()
        != Some(expected)
    {
        return Err(EmbeddingError::Transport(
            "selected semantic worker no longer owns its service".into(),
        ));
    }
    Ok(())
}

pub struct ServiceEmbeddingProvider {
    registry: Arc<ServiceWorkerRegistry>,
    expected: ServiceModel,
    provider: Arc<dyn EmbeddingProvider>,
}

impl ServiceEmbeddingProvider {
    pub fn new(
        registry: Arc<ServiceWorkerRegistry>,
        expected: ServiceModel,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self, EmbeddingError> {
        if expected.service != AiService::Embedding {
            return Err(EmbeddingError::Configuration(
                "embedding provider requires the Embedding service".into(),
            ));
        }
        assert_selected(&registry, &expected)?;
        Ok(Self {
            registry,
            expected,
            provider,
        })
    }

    async fn execute<T, F>(&self, operation: impl FnOnce() -> F) -> Result<T, EmbeddingError>
    where
        F: std::future::Future<Output = Result<T, EmbeddingError>> + Send,
    {
        let _permit = self.registry.inference_permit(AiService::Embedding).await;
        assert_selected(&self.registry, &self.expected)?;
        let result = operation().await?;
        assert_selected(&self.registry, &self.expected)?;
        Ok(result)
    }
}

impl EmbeddingProvider for ServiceEmbeddingProvider {
    fn space(&self) -> &EmbeddingSpace {
        self.provider.space()
    }

    fn document_request_count(&self, document_count: usize) -> usize {
        self.provider.document_request_count(document_count)
    }

    fn embed(&self, purpose: EmbeddingPurpose, text: String) -> EmbeddingFuture<'_, Embedding> {
        Box::pin(self.execute(move || self.provider.embed(purpose, text)))
    }

    fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
        Box::pin(self.execute(move || self.provider.embed_documents(texts)))
    }
}

pub struct ServiceRerankProvider {
    registry: Arc<ServiceWorkerRegistry>,
    expected: ServiceModel,
    provider: Arc<dyn RerankProvider>,
}

impl ServiceRerankProvider {
    pub fn new(
        registry: Arc<ServiceWorkerRegistry>,
        expected: ServiceModel,
        provider: Arc<dyn RerankProvider>,
    ) -> Result<Self, EmbeddingError> {
        if expected.service != AiService::Reranker {
            return Err(EmbeddingError::Configuration(
                "reranker provider requires the Reranker service".into(),
            ));
        }
        assert_selected(&registry, &expected)?;
        Ok(Self {
            registry,
            expected,
            provider,
        })
    }
}

impl RerankProvider for ServiceRerankProvider {
    fn score<'a>(&'a self, query: &'a str, candidates: &'a [String]) -> RerankFuture<'a> {
        Box::pin(async move {
            let _permit = self.registry.inference_permit(AiService::Reranker).await;
            assert_selected(&self.registry, &self.expected)?;
            let result = self.provider.score(query, candidates).await?;
            assert_selected(&self.registry, &self.expected)?;
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::ai_runtime::model_identity::{AiModelIdentity, WorkerImplementation};

    fn model(service: AiService, id: &str) -> ServiceModel {
        ServiceModel {
            service,
            model: AiModelIdentity {
                model_id: id.into(),
                implementation: WorkerImplementation::Python,
                revision: None,
            },
        }
    }

    struct MockEmbedding {
        space: EmbeddingSpace,
        calls: AtomicUsize,
        started: Arc<tokio::sync::Notify>,
        continue_call: Arc<tokio::sync::Notify>,
    }

    impl EmbeddingProvider for MockEmbedding {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(
            &self,
            _purpose: EmbeddingPurpose,
            _text: String,
        ) -> EmbeddingFuture<'_, Embedding> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                self.continue_call.notified().await;
                Embedding::new(vec![1., 0.], &self.space)
            })
        }

        fn embed_documents(&self, texts: Vec<String>) -> EmbeddingFuture<'_, Vec<Embedding>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                self.continue_call.notified().await;
                texts
                    .iter()
                    .map(|_| Embedding::new(vec![1., 0.], &self.space))
                    .collect()
            })
        }
    }

    #[tokio::test]
    async fn embedding_batch_holds_one_service_permit_for_entire_remote_operation() {
        let registry = ServiceWorkerRegistry::new();
        let selected = model(AiService::Embedding, "encoder");
        let claim = registry.claim(selected.clone()).unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let backend = Arc::new(MockEmbedding {
            space: EmbeddingSpace::new("python", "encoder", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
            started: Arc::clone(&started),
            continue_call: Arc::clone(&resume),
        });
        let worker = Arc::new(
            ServiceEmbeddingProvider::new(Arc::clone(&registry), selected, backend.clone())
                .unwrap(),
        );
        let task =
            tokio::spawn(async move { worker.embed_documents(vec!["a".into(), "b".into()]).await });
        started.notified().await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(30),
                registry.transition_gate(AiService::Embedding)
            )
            .await
            .is_err()
        );
        let independent = registry.transition_gate(AiService::Reranker).await;
        drop(independent);
        resume.notify_one();
        assert_eq!(task.await.unwrap().unwrap().len(), 2);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        let transition = registry.transition_gate(AiService::Embedding).await;
        drop(transition);
        drop(claim);
    }

    #[tokio::test]
    async fn stale_or_foreign_model_fails_before_embedding_provider_dispatch() {
        let registry = ServiceWorkerRegistry::new();
        let claim = registry
            .claim(model(AiService::Embedding, "actual"))
            .unwrap();
        let backend = Arc::new(MockEmbedding {
            space: EmbeddingSpace::new("python", "encoder", "v1", 2).unwrap(),
            calls: AtomicUsize::new(0),
            started: Arc::new(tokio::sync::Notify::new()),
            continue_call: Arc::new(tokio::sync::Notify::new()),
        });
        assert!(
            ServiceEmbeddingProvider::new(
                registry.clone(),
                model(AiService::Embedding, "other"),
                backend.clone()
            )
            .is_err()
        );
        assert!(
            ServiceEmbeddingProvider::new(
                registry.clone(),
                model(AiService::Tts, "actual"),
                backend.clone()
            )
            .is_err()
        );
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        drop(claim);
    }

    struct EagerProvider {
        calls: AtomicUsize,
        space: EmbeddingSpace,
    }

    impl EmbeddingProvider for EagerProvider {
        fn space(&self) -> &EmbeddingSpace {
            &self.space
        }

        fn embed(
            &self,
            _purpose: EmbeddingPurpose,
            _text: String,
        ) -> EmbeddingFuture<'_, Embedding> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Embedding::new(vec![1., 0.], &self.space) })
        }
    }

    #[tokio::test]
    async fn no_provider_method_is_invoked_until_transition_gate_releases() {
        let registry = ServiceWorkerRegistry::new();
        let selected = model(AiService::Embedding, "same");
        let _claim = registry.claim(selected.clone()).unwrap();
        let backend = Arc::new(EagerProvider {
            calls: AtomicUsize::new(0),
            space: EmbeddingSpace::new("python", "same", "v1", 2).unwrap(),
        });
        let routed = Arc::new(
            ServiceEmbeddingProvider::new(registry.clone(), selected, backend.clone()).unwrap(),
        );
        let gate = registry.transition_gate(AiService::Embedding).await;
        let task =
            tokio::spawn(
                async move { routed.embed(EmbeddingPurpose::Query, "hello".into()).await },
            );
        tokio::task::yield_now().await;
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
        drop(gate);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    struct MockReranker(AtomicUsize);

    impl RerankProvider for MockReranker {
        fn score<'a>(&'a self, _query: &'a str, _candidates: &'a [String]) -> RerankFuture<'a> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(vec![0.5])
            })
        }
    }

    #[tokio::test]
    async fn reranker_uses_distinct_slot_and_rejects_replaced_model() {
        let registry = ServiceWorkerRegistry::new();
        let claim = registry.claim(model(AiService::Reranker, "v1")).unwrap();
        let inner = Arc::new(MockReranker(AtomicUsize::new(0)));
        let routed = ServiceRerankProvider::new(
            registry.clone(),
            model(AiService::Reranker, "v1"),
            inner.clone(),
        )
        .unwrap();
        assert_eq!(
            routed.score("query", &["candidate".into()]).await.unwrap(),
            vec![0.5]
        );
        assert_eq!(inner.0.load(Ordering::SeqCst), 1);
        let gate = registry.transition_gate(AiService::Reranker).await;
        drop(claim);
        let _replacement = registry.claim(model(AiService::Reranker, "v2")).unwrap();
        drop(gate);
        assert!(routed.score("query", &["candidate".into()]).await.is_err());
        assert_eq!(inner.0.load(Ordering::SeqCst), 1);
    }
}
