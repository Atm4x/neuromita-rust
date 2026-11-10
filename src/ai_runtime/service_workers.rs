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
}
