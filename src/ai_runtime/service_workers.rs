//! The process ownership boundary is keyed by AI service, not by implementation.
//! A model implementation is immutable catalog metadata, never an engine toggle.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use thiserror::Error;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use uuid::Uuid;

use super::{AiService, model_identity::AiModelIdentity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceModel {
    pub service: AiService,
    pub model: AiModelIdentity,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorkerOwnershipError {
    #[error("the requested service already owns an active worker for a different model")]
    AlreadyActive,
    #[error("AI worker registry is poisoned")]
    Poisoned,
    #[error("the selected worker session is not active or has been replaced")]
    StaleSession,
}

pub struct ServiceWorkerRegistry {
    active: Arc<Mutex<HashMap<AiService, (Uuid, ServiceModel)>>>,
    gates: HashMap<AiService, RwLock<()>>,
}

/// Evidence that a specific service's transition fence is held. A transport
/// may not dispatch native inference using another service's permit.
pub(crate) struct ServiceInferencePermit<'a> {
    owner: &'a ServiceWorkerRegistry,
    service: AiService,
    _guard: RwLockReadGuard<'a, ()>,
}

impl ServiceInferencePermit<'_> {
    pub(crate) fn authorizes(&self, owner: &ServiceWorkerRegistry, service: AiService) -> bool {
        self.service == service && std::ptr::eq(self.owner, owner)
    }
}

impl Default for ServiceWorkerRegistry {
    fn default() -> Self {
        let services = [
            AiService::Tts,
            AiService::Asr,
            AiService::Embedding,
            AiService::Reranker,
            AiService::Beats,
        ];
        Self {
            active: Arc::new(Mutex::new(HashMap::new())),
            gates: services
                .into_iter()
                .map(|service| (service, RwLock::new(())))
                .collect(),
        }
    }
}

impl ServiceWorkerRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Inference keeps a read permit through the external provider call and
    /// its final validation. A service transition cannot pass it.
    pub async fn inference_gate(&self, service: AiService) -> RwLockReadGuard<'_, ()> {
        self.gates[&service].read().await
    }

    pub(crate) async fn inference_permit(&self, service: AiService) -> ServiceInferencePermit<'_> {
        ServiceInferencePermit {
            owner: self,
            service,
            _guard: self.inference_gate(service).await,
        }
    }

    /// The owner holds an exclusive permit through shutdown, admission,
    /// activation and publication of its next process generation.
    pub async fn transition_gate(&self, service: AiService) -> RwLockWriteGuard<'_, ()> {
        self.gates[&service].write().await
    }

    /// Bind one typed operation port to the exact active process generation. The
    /// port is not a second process owner: `WorkerClaim` remains the sole owner.
    /// Rebinding after restart is explicit, even if the model ID is unchanged.
    pub fn connect<B: ?Sized>(
        self: &Arc<Self>,
        expected: ServiceModel,
        backend: Arc<B>,
    ) -> Result<ServiceWorker<B>, WorkerOwnershipError> {
        let generation = self
            .active
            .lock()
            .map_err(|_| WorkerOwnershipError::Poisoned)?
            .get(&expected.service)
            .and_then(|(generation, model)| (model == &expected).then_some(*generation))
            .ok_or(WorkerOwnershipError::StaleSession)?;
        Ok(ServiceWorker {
            registry: Arc::clone(self),
            expected,
            generation,
            backend,
        })
    }

    /// Claim before spawning. The claim lasts until the process has terminated;
    /// no two implementations or revisions can occupy the same service at once.
    pub fn claim(&self, model: ServiceModel) -> Result<WorkerClaim, WorkerOwnershipError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| WorkerOwnershipError::Poisoned)?;
        if active.contains_key(&model.service) {
            return Err(WorkerOwnershipError::AlreadyActive);
        }
        let generation = Uuid::new_v4();
        active.insert(model.service, (generation, model.clone()));
        Ok(WorkerClaim {
            active: Arc::clone(&self.active),
            service: model.service,
            generation,
        })
    }

    pub fn active(&self, service: AiService) -> Result<Option<ServiceModel>, WorkerOwnershipError> {
        Ok(self
            .active
            .lock()
            .map_err(|_| WorkerOwnershipError::Poisoned)?
            .get(&service)
            .map(|(_, model)| model.clone()))
    }
}

/// The common service-worker API. It binds an existing typed worker port to a
/// single service slot and process generation without knowing its implementation.
/// The registry owns synchronization, while `WorkerClaim` owns the process slot.
pub struct ServiceWorker<B: ?Sized> {
    registry: Arc<ServiceWorkerRegistry>,
    expected: ServiceModel,
    generation: Uuid,
    backend: Arc<B>,
}

impl<B: ?Sized> ServiceWorker<B> {
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// A call pins the session across transport I/O. Transitions for this
    /// service wait for the call, but unrelated services proceed independently.
    pub async fn begin(&self) -> Result<WorkerInvocation<'_, B>, WorkerOwnershipError> {
        let permit = self.registry.inference_permit(self.expected.service).await;
        self.assert_current()?;
        Ok(WorkerInvocation {
            worker: self,
            permit,
        })
    }

    fn assert_current(&self) -> Result<(), WorkerOwnershipError> {
        let active = self
            .registry
            .active
            .lock()
            .map_err(|_| WorkerOwnershipError::Poisoned)?;
        if active.get(&self.expected.service) != Some(&(self.generation, self.expected.clone())) {
            return Err(WorkerOwnershipError::StaleSession);
        }
        Ok(())
    }
}

pub struct WorkerInvocation<'a, B: ?Sized> {
    worker: &'a ServiceWorker<B>,
    permit: ServiceInferencePermit<'a>,
}

impl<B: ?Sized> WorkerInvocation<'_, B> {
    pub fn backend(&self) -> &B {
        self.worker.backend()
    }

    pub(crate) fn permit(&self) -> &ServiceInferencePermit<'_> {
        &self.permit
    }

    /// Must be called after transport completion, before exposing a result.
    /// Even a release/reclaim of the same model identity is a stale session.
    pub fn complete<T>(&self, output: T) -> Result<T, WorkerOwnershipError> {
        self.worker.assert_current()?;
        Ok(output)
    }

    /// Execute a typed operation under the same admission for its whole await.
    /// The outer result is worker ownership; the inner result is the operation.
    /// Neither cancellation nor transport failure replaces the active worker.
    pub async fn dispatch<T, E>(
        &self,
        operation: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<Result<T, E>, WorkerOwnershipError> {
        let outcome = operation.await;
        self.worker.assert_current()?;
        Ok(outcome)
    }
}

pub struct WorkerClaim {
    active: Arc<Mutex<HashMap<AiService, (Uuid, ServiceModel)>>>,
    service: AiService,
    generation: Uuid,
}

impl Drop for WorkerClaim {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            if active
                .get(&self.service)
                .is_some_and(|(generation, _)| *generation == self.generation)
            {
                active.remove(&self.service);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SpeechModelReference;

    fn model(service: AiService, model: SpeechModelReference) -> ServiceModel {
        ServiceModel {
            service,
            model: model.into(),
        }
    }

    #[test]
    fn one_model_per_service_even_with_duplicate_name_and_different_implementation() {
        let registry = ServiceWorkerRegistry::new();
        let python = model(AiService::Tts, SpeechModelReference::python("fish"));
        let rust = model(
            AiService::Tts,
            SpeechModelReference::rust("fish", "f".repeat(64)),
        );
        let first = registry.claim(python.clone()).unwrap();
        assert_eq!(registry.active(AiService::Tts).unwrap(), Some(python));
        assert!(matches!(
            registry.claim(rust.clone()),
            Err(WorkerOwnershipError::AlreadyActive)
        ));
        let asr = registry
            .claim(model(
                AiService::Asr,
                SpeechModelReference::python("whisper"),
            ))
            .unwrap();
        assert!(registry.active(AiService::Asr).unwrap().is_some());
        drop(first);
        let replacement = registry.claim(rust.clone()).unwrap();
        assert_eq!(registry.active(AiService::Tts).unwrap(), Some(rust));
        drop(replacement);
        assert_eq!(registry.active(AiService::Tts).unwrap(), None);
        drop(asr);
        assert_eq!(registry.active(AiService::Asr).unwrap(), None);
    }

    #[test]
    fn one_slot_is_independent_of_other_services() {
        let registry = ServiceWorkerRegistry::new();
        let tts = registry
            .claim(model(AiService::Tts, SpeechModelReference::python("same")))
            .unwrap();
        let asr = registry
            .claim(model(
                AiService::Asr,
                SpeechModelReference::rust("same", "e".repeat(64)),
            ))
            .unwrap();
        assert_eq!(
            registry
                .active(AiService::Tts)
                .unwrap()
                .unwrap()
                .model
                .model_id,
            "same"
        );
        assert_eq!(
            registry
                .active(AiService::Asr)
                .unwrap()
                .unwrap()
                .model
                .model_id,
            "same"
        );
        drop((tts, asr));
    }
    #[test]
    fn non_speech_services_share_model_identity_without_sharing_worker_slots() {
        let registry = ServiceWorkerRegistry::new();
        let embedding = registry
            .claim(ServiceModel {
                service: AiService::Embedding,
                model: SpeechModelReference::python("encoder").into(),
            })
            .unwrap();
        let reranker = registry
            .claim(ServiceModel {
                service: AiService::Reranker,
                model: SpeechModelReference::rust("encoder", "a".repeat(64)).into(),
            })
            .unwrap();
        assert_eq!(
            registry
                .active(AiService::Embedding)
                .unwrap()
                .unwrap()
                .model
                .model_id,
            "encoder"
        );
        assert_eq!(
            registry
                .active(AiService::Reranker)
                .unwrap()
                .unwrap()
                .model
                .model_id,
            "encoder"
        );
        assert!(
            registry
                .claim(ServiceModel {
                    service: AiService::Embedding,
                    model: SpeechModelReference::rust("encoder", "a".repeat(64)).into(),
                })
                .is_err()
        );
        drop((embedding, reranker));
        assert!(registry.active(AiService::Embedding).unwrap().is_none());
    }

    #[tokio::test]
    async fn inference_permit_is_bound_to_both_owner_and_service() {
        let first = ServiceWorkerRegistry::new();
        let other = ServiceWorkerRegistry::new();
        let permit = first.inference_permit(AiService::Tts).await;
        assert!(permit.authorizes(&first, AiService::Tts));
        assert!(!permit.authorizes(&first, AiService::Asr));
        assert!(!permit.authorizes(&other, AiService::Tts));
    }

    #[tokio::test]
    async fn retiring_a_worker_cannot_overtake_an_inflight_request() {
        let registry = ServiceWorkerRegistry::new();
        let inference = registry.inference_gate(AiService::Tts).await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                registry.transition_gate(AiService::Tts)
            )
            .await
            .is_err()
        );
        let independent = registry.transition_gate(AiService::Asr).await;
        drop(independent);
        drop(inference);
        let transition = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            registry.transition_gate(AiService::Tts),
        )
        .await
        .unwrap();
        drop(transition);
    }
    #[tokio::test]
    async fn typed_port_is_pinned_to_exact_generation_even_after_same_model_restarts() {
        let registry = ServiceWorkerRegistry::new();
        let selected = model(AiService::Embedding, SpeechModelReference::python("same"));
        let owner = registry.claim(selected.clone()).unwrap();
        let port = registry
            .connect(selected.clone(), Arc::new(42usize))
            .unwrap();
        let call = port.begin().await.unwrap();
        assert_eq!(*call.backend(), 42);
        assert_eq!(call.complete(123).unwrap(), 123);
        drop(call);
        let transition = registry.transition_gate(AiService::Embedding).await;
        drop(owner);
        let _new_owner = registry.claim(selected.clone()).unwrap();
        drop(transition);
        assert!(matches!(
            port.begin().await,
            Err(WorkerOwnershipError::StaleSession)
        ));
        assert!(registry.connect(selected, Arc::new(43usize)).is_ok());
    }

    #[tokio::test]
    async fn cancelled_typed_operation_releases_its_service_gate() {
        let registry = ServiceWorkerRegistry::new();
        let selected = model(
            AiService::Embedding,
            SpeechModelReference::python("embedding"),
        );
        let _owner = registry.claim(selected.clone()).unwrap();
        let worker = Arc::new(registry.connect(selected, Arc::new(())).unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let call = worker.begin().await.unwrap();
            started_tx.send(()).unwrap();
            call.dispatch(std::future::pending::<Result<(), ()>>())
                .await
        });
        started_rx.await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(25),
                registry.transition_gate(AiService::Embedding)
            )
            .await
            .is_err()
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let gate = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            registry.transition_gate(AiService::Embedding),
        )
        .await
        .unwrap();
        drop(gate);
    }

    #[tokio::test]
    async fn typed_worker_call_fences_replacement_but_not_other_services() {
        let registry = ServiceWorkerRegistry::new();
        let selected = model(AiService::Tts, SpeechModelReference::python("voice"));
        let _owner = registry.claim(selected.clone()).unwrap();
        let port = registry.connect(selected, Arc::new(())).unwrap();
        let call = port.begin().await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(25),
                registry.transition_gate(AiService::Tts)
            )
            .await
            .is_err()
        );
        let asr_transition = registry.transition_gate(AiService::Asr).await;
        drop(asr_transition);
        drop(call);
        let tts_transition = registry.transition_gate(AiService::Tts).await;
        drop(tts_transition);
    }
}
