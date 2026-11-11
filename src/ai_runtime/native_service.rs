//! Core-owned native worker and package operation lifecycle.
//! The desktop sends commands and observes read models, but never holds a worker or download task.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    native_download_operation::{
        NativeDownloadEvent, NativeDownloadOperation, NativeDownloadSelection,
    },
    native_https::NativeHttpsOrigin,
    native_model::{NativeBackend, NativeModelSnapshot},
    native_revisions::{NativeRevisionOverview, NativeRevisionStore},
    native_speech::NativeSpeechKind,
    native_trust::{NativeAuthorizedRelease, NativePolicyPin, NativeReleaseTrust},
    native_workflow::{NativeModelWorkflow, NativeWorkflowSnapshot},
    service_workers::{ServiceInferencePermit, ServiceModel, ServiceWorkerRegistry, WorkerClaim},
};

struct NativeState {
    workflow: Option<NativeModelWorkflow>,
    download: Option<NativeDownloadOperation>,
    claims: HashMap<NativeSpeechKind, WorkerClaim>,
}

pub struct AiHubNativeService {
    root: PathBuf,
    state: Mutex<NativeState>,
    stop: CancellationToken,
    registry: Arc<ServiceWorkerRegistry>,
}

pub fn native_download_configured() -> bool {
    publisher_policy_pin().is_ok_and(|pin| pin.is_some())
        && option_env!("NEUROMITA_NATIVE_PACKAGE_ORIGIN")
            .is_some_and(|origin| NativeHttpsOrigin::parse(origin).is_ok())
}

fn publisher_policy_pin() -> Result<Option<NativePolicyPin>, String> {
    option_env!("NEUROMITA_NATIVE_PUBLISHER_POLICY_SHA256")
        .map(NativePolicyPin::from_sha256_hex)
        .transpose()
        .map_err(|error| error.to_string())
}

fn trust(root: &Path) -> Result<NativeReleaseTrust, String> {
    Ok(NativeReleaseTrust::new(
        root.join("Config/native-publisher-keys.json"),
        root.join("AIData/NativeApprovals"),
        publisher_policy_pin()?,
    ))
}

fn empty_snapshot() -> NativeWorkflowSnapshot {
    NativeWorkflowSnapshot {
        trust: Vec::new(),
        inventory: NativeRevisionOverview {
            revisions: Vec::new(),
            selections: Vec::new(),
        },
        active_revision: None,
        worker: None,
        service_workers: Vec::new(),
    }
}

fn open_workflow(root: &Path) -> Result<Option<NativeModelWorkflow>, String> {
    let revisions = root.join("AIData/NativeModels/revisions");
    let runtime = root.join("AIData/NativeRuntime");
    let revisions_exist = revisions.try_exists().map_err(|error| error.to_string())?;
    let runtime_exists = runtime.try_exists().map_err(|error| error.to_string())?;
    if !revisions_exist && !runtime_exists {
        return Ok(None);
    }
    if !revisions_exist || !runtime_exists {
        return Err("Нативное хранилище установлено не полностью".into());
    }
    let workflow =
        NativeModelWorkflow::with_trust(NativeRevisionStore::new(revisions, runtime), trust(root)?);
    workflow.snapshot().map_err(|error| error.to_string())?;
    Ok(Some(workflow))
}

fn ensure_storage(path: &Path) -> Result<(), String> {
    for part in path.ancestors() {
        if let Ok(metadata) = std::fs::symlink_metadata(part)
            && (metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            return Err("Нативное хранилище содержит небезопасный каталог".into());
        }
    }
    std::fs::create_dir_all(path).map_err(|error| error.to_string())
}

impl AiHubNativeService {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Self::with_registry(root, ServiceWorkerRegistry::new())
    }

    pub fn with_registry(root: PathBuf, registry: Arc<ServiceWorkerRegistry>) -> Arc<Self> {
        Arc::new(Self {
            root,
            state: Mutex::new(NativeState {
                workflow: None,
                download: None,
                claims: HashMap::new(),
            }),
            stop: CancellationToken::new(),
            registry,
        })
    }

    pub async fn snapshot(&self) -> Result<NativeWorkflowSnapshot, String> {
        let state = self.state.lock().await;
        match &state.workflow {
            Some(workflow) => workflow.snapshot().map_err(|error| error.to_string()),
            None => Ok(empty_snapshot()),
        }
    }

    pub async fn refresh(&self) -> Result<NativeWorkflowSnapshot, String> {
        let mut state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        if state.workflow.is_none() {
            state.workflow = open_workflow(&self.root)?;
        }
        match &state.workflow {
            Some(workflow) => workflow.snapshot().map_err(|error| error.to_string()),
            None => Ok(empty_snapshot()),
        }
    }

    pub fn authorized_releases(&self) -> Result<Vec<NativeAuthorizedRelease>, String> {
        trust(&self.root)?
            .available_releases()
            .map_err(|error| error.to_string())
    }

    pub async fn select(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        backend: NativeBackend,
    ) -> Result<(), String> {
        let state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        state
            .workflow
            .as_ref()
            .ok_or("Нативная ревизия недоступна")?
            .select_trusted(model_id, manifest_sha256, backend)
            .map_err(|error| error.to_string())
    }

    pub async fn activate(&self, model_id: &str) -> Result<NativeModelSnapshot, String> {
        let services = {
            let state = self.state.lock().await;
            if self.stop.is_cancelled() {
                return Err("Приложение завершает работу".into());
            }
            state
                .workflow
                .as_ref()
                .ok_or("Нативная ревизия недоступна")?
                .selected_speech_services(model_id)
                .map_err(|e| e.to_string())?
        };
        let _tts_gate = if services == [NativeSpeechKind::Tts] {
            Some(self.registry.transition_gate(super::AiService::Tts).await)
        } else {
            None
        };
        let _asr_gate = if services == [NativeSpeechKind::Asr] {
            Some(self.registry.transition_gate(super::AiService::Asr).await)
        } else {
            None
        };
        let mut state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        match services.as_slice() {
            [] => state
                .workflow
                .as_mut()
                .ok_or("Нативная ревизия недоступна")?
                .activate_selected(model_id, Duration::from_secs(30))
                .await
                .map_err(|e| e.to_string()),
            [service] => {
                let service = *service;
                drop(state);
                self.activate_speech(service, model_id, None).await
            }
            _ => Err(
                "Для модели с несколькими сервисами необходима привязка к конкретному сервису"
                    .into(),
            ),
        }
    }

    async fn activate_speech(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
        expected_revision: Option<[u8; 32]>,
    ) -> Result<NativeModelSnapshot, String> {
        let service_id = match service {
            NativeSpeechKind::Tts => super::AiService::Tts,
            NativeSpeechKind::Asr => super::AiService::Asr,
        };
        let (prepared, choice) = {
            let state = self.state.lock().await;
            if self.stop.is_cancelled() {
                return Err("Приложение завершает работу".into());
            }
            let workflow = state
                .workflow
                .as_ref()
                .ok_or("Нативная ревизия недоступна")?;
            let prepared = workflow
                .prepare_for_service(service, model_id, Duration::from_secs(30))
                .map_err(|e| e.to_string())?;
            let choice = prepared.choice();
            if expected_revision.is_some_and(|digest| choice.manifest_sha256 != digest) {
                return Err("Выбранная ревизия изменилась до запуска worker".into());
            }
            (prepared, choice)
        };
        let model = crate::settings::SpeechModelReference::rust(
            model_id,
            choice
                .manifest_sha256
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        );
        let claim = self
            .registry
            .claim(ServiceModel {
                service: service_id,
                model: model.into(),
            })
            .map_err(|e| e.to_string())?;
        let (session, snapshot) = prepared.start().await.map_err(|e| e.to_string())?;
        let mut state = self.state.lock().await;
        if state.workflow.is_none() {
            session.deactivate().await.map_err(|e| e.to_string())?;
            return Err("Нативная ревизия недоступна".into());
        }
        if let Err(error) = state
            .workflow
            .as_mut()
            .unwrap()
            .install_active_speech(Arc::clone(&session))
        {
            session.deactivate().await.map_err(|e| e.to_string())?;
            return Err(error.to_string());
        }
        state.claims.insert(service, claim);
        Ok(snapshot)
    }

    pub async fn activate_for_service(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
    ) -> Result<NativeModelSnapshot, String> {
        self.activate_for_service_pinned(service, model_id, None)
            .await
    }

    pub async fn activate_exact_for_service(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
        revision: &str,
    ) -> Result<NativeModelSnapshot, String> {
        let digest = super::native_registry::parse_sha256(revision).map_err(|e| e.to_string())?;
        self.activate_for_service_pinned(service, model_id, Some(digest))
            .await
    }

    pub(crate) async fn validate_service_selection(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
        revision: &str,
    ) -> Result<(), String> {
        let digest =
            super::native_registry::parse_sha256(revision).map_err(|error| error.to_string())?;
        let state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        let prepared = state
            .workflow
            .as_ref()
            .ok_or("Нативная ревизия недоступна")?
            .prepare_for_service(service, model_id, Duration::from_secs(30))
            .map_err(|error| error.to_string())?;
        if prepared.choice().manifest_sha256 != digest {
            return Err("Выбранная ревизия изменилась".into());
        }
        Ok(())
    }

    async fn activate_for_service_pinned(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
        expected_revision: Option<[u8; 32]>,
    ) -> Result<NativeModelSnapshot, String> {
        let ai_service = match service {
            NativeSpeechKind::Tts => super::AiService::Tts,
            NativeSpeechKind::Asr => super::AiService::Asr,
        };
        let _transition = self.registry.transition_gate(ai_service).await;
        let state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        let capabilities = state
            .workflow
            .as_ref()
            .ok_or("Нативная ревизия недоступна")?
            .selected_speech_services(model_id)
            .map_err(|e| e.to_string())?;
        if !capabilities.contains(&service) {
            return Err("Модель не поддерживает запрошенный сервис".into());
        }
        drop(state);
        self.activate_speech(service, model_id, expected_revision)
            .await
    }

    pub async fn deactivate_for_service(&self, service: NativeSpeechKind) -> Result<(), String> {
        self.deactivate_for_service_pinned(service, None).await
    }

    pub async fn deactivate_exact_for_service(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
        revision: &str,
    ) -> Result<(), String> {
        let digest = super::native_registry::parse_sha256(revision).map_err(|e| e.to_string())?;
        self.deactivate_for_service_pinned(service, Some((model_id, digest)))
            .await
    }

    async fn deactivate_for_service_pinned(
        &self,
        service: NativeSpeechKind,
        expected: Option<(&str, [u8; 32])>,
    ) -> Result<(), String> {
        let ai_service = match service {
            NativeSpeechKind::Tts => super::AiService::Tts,
            NativeSpeechKind::Asr => super::AiService::Asr,
        };
        let _transition = self.registry.transition_gate(ai_service).await;
        let session = {
            let state = self.state.lock().await;
            if self.stop.is_cancelled() {
                return Err("Приложение завершает работу".into());
            }
            let session = state
                .workflow
                .as_ref()
                .ok_or("Нативная сессия недоступна")?
                .speech_session(service)
                .map_err(|error| error.to_string())?;
            if expected.is_some_and(|(model_id, digest)| {
                let actual = session.choice();
                actual.model_id != model_id || actual.manifest_sha256 != digest
            }) {
                return Err("Активная ревизия сервиса изменилась до остановки".into());
            }
            session
        };
        session
            .deactivate()
            .await
            .map_err(|error| error.to_string())?;
        let mut state = self.state.lock().await;
        state
            .workflow
            .as_mut()
            .ok_or("Нативная сессия недоступна")?
            .unpublish_stopped_speech(service, &session)
            .map_err(|error| error.to_string())?;
        state.claims.remove(&service);
        Ok(())
    }

    pub(crate) async fn synthesize_speech(
        &self,
        permit: &ServiceInferencePermit<'_>,
        model_id: &str,
        manifest_sha256: &str,
        text: &str,
        voice: &str,
        language: &str,
        volume_percent: u8,
    ) -> Result<crate::voice::AudioArtifact, String> {
        if !permit.authorizes(self.registry.as_ref(), super::AiService::Tts) {
            return Err("Inference permit does not belong to the TTS owner".into());
        }
        let session = {
            let state = self.state.lock().await;
            if self.stop.is_cancelled() {
                return Err("Приложение завершает работу".into());
            }
            state
                .workflow
                .as_ref()
                .ok_or("Нативная сессия недоступна")?
                .speech_session(NativeSpeechKind::Tts)
                .map_err(|error| error.to_string())?
        };
        session
            .synthesize(
                model_id,
                manifest_sha256,
                text,
                voice,
                language,
                volume_percent,
            )
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn transcribe_speech(
        &self,
        permit: &ServiceInferencePermit<'_>,
        model_id: &str,
        manifest_sha256: &str,
        wav: &[u8],
        language: &str,
    ) -> Result<crate::ai_runtime::speech::Transcript, String> {
        if !permit.authorizes(self.registry.as_ref(), super::AiService::Asr) {
            return Err("Inference permit does not belong to the ASR owner".into());
        }
        let session = {
            let state = self.state.lock().await;
            if self.stop.is_cancelled() {
                return Err("Приложение завершает работу".into());
            }
            state
                .workflow
                .as_ref()
                .ok_or("Нативная сессия недоступна")?
                .speech_session(NativeSpeechKind::Asr)
                .map_err(|error| error.to_string())?
        };
        session
            .transcribe(model_id, manifest_sha256, wav, language)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn deactivate(&self) -> Result<(), String> {
        let _tts_gate = self.registry.transition_gate(super::AiService::Tts).await;
        let _asr_gate = self.registry.transition_gate(super::AiService::Asr).await;
        let mut state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        state
            .workflow
            .as_mut()
            .ok_or("Нативная сессия недоступна")?
            .deactivate_all()
            .await
            .map_err(|error| error.to_string())?;
        state.claims.clear();
        Ok(())
    }

    pub async fn refresh_worker(&self) -> Result<(), String> {
        let (tts, asr) = {
            let mut state = self.state.lock().await;
            let Some(workflow) = state.workflow.as_mut() else {
                return Ok(());
            };
            workflow
                .refresh_generic_worker()
                .await
                .map_err(|error| error.to_string())?;
            (
                workflow.speech_session(NativeSpeechKind::Tts).ok(),
                workflow.speech_session(NativeSpeechKind::Asr).ok(),
            )
        };
        let tts_refresh = async {
            if let Some(worker) = tts {
                worker.refresh().await.map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        };
        let asr_refresh = async {
            if let Some(worker) = asr {
                worker.refresh().await.map_err(|error| error.to_string())?;
            }
            Ok::<(), String>(())
        };
        let (tts_result, asr_result) = tokio::join!(tts_refresh, asr_refresh);
        tts_result?;
        asr_result
    }

    pub async fn start_download(
        &self,
        request: NativeDownloadSelection,
        events: mpsc::UnboundedSender<NativeDownloadEvent>,
    ) -> Result<Uuid, String> {
        let mut state = self.state.lock().await;
        if self.stop.is_cancelled() {
            return Err("Приложение завершает работу".into());
        }
        if state.download.is_some() {
            return Err("Установка уже выполняется".into());
        }
        let origin = option_env!("NEUROMITA_NATIVE_PACKAGE_ORIGIN")
            .ok_or_else(|| "Источник нативных пакетов не закреплён в сборке".to_owned())
            .and_then(|value| NativeHttpsOrigin::parse(value).map_err(|error| error.to_string()))?;
        let pin =
            publisher_policy_pin()?.ok_or("В сборке отсутствует доверенная политика издателей")?;
        let trust = NativeReleaseTrust::new(
            self.root.join("Config/native-publisher-keys.json"),
            self.root.join("AIData/NativeApprovals"),
            Some(pin),
        );
        trust
            .verify_hex(&request.model_id, &request.manifest_sha256)
            .map_err(|error| error.to_string())?;
        let revisions = self.root.join("AIData/NativeModels/revisions");
        let runtime = self.root.join("AIData/NativeRuntime");
        let staging = self.root.join("AIData/NativeDownloads");
        for path in [&revisions, &runtime, &staging] {
            ensure_storage(path)?;
        }
        let operation = NativeDownloadOperation::start(
            NativeRevisionStore::new(revisions, runtime),
            trust,
            origin,
            request,
            staging,
            Duration::from_secs(3600),
            events,
        )
        .map_err(|error| error.to_string())?;
        let id = operation.id();
        state.download = Some(operation);
        Ok(id)
    }

    pub async fn cancel_download(&self) -> bool {
        let state = self.state.lock().await;
        if let Some(operation) = state.download.as_ref() {
            operation.cancel();
            true
        } else {
            false
        }
    }

    pub async fn finish_download(&self, id: Uuid) -> bool {
        let mut state = self.state.lock().await;
        if state
            .download
            .as_ref()
            .is_none_or(|operation| operation.id() != id)
        {
            return false;
        }
        if let Some(mut operation) = state.download.take() {
            operation.join().await;
        }
        true
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        let _tts_gate = self.registry.transition_gate(super::AiService::Tts).await;
        let _asr_gate = self.registry.transition_gate(super::AiService::Asr).await;
        let mut state = self.state.lock().await;
        if let Some(mut operation) = state.download.take() {
            operation.cancel();
            operation.join().await;
        }
        if let Some(workflow) = state.workflow.as_mut() {
            if let Err(error) = workflow.deactivate_all().await {
                tracing::warn!(%error, "native worker shutdown failed");
            }
        }
        state.claims.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_native_owner_is_not_a_ready_worker() {
        let root = std::env::temp_dir().join(format!("native-owner-empty-{}", Uuid::new_v4()));
        let owner = AiHubNativeService::new(root.clone());
        let snapshot = owner.refresh().await.unwrap();
        assert!(snapshot.worker.is_none());
        assert!(snapshot.inventory.revisions.is_empty());
        assert!(owner.activate("arbitrary").await.is_err());
        owner.shutdown().await;
        assert!(owner.refresh().await.is_err());
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn download_without_pinned_origin_is_rejected_before_touching_storage() {
        let root = std::env::temp_dir().join(format!("native-owner-untrusted-{}", Uuid::new_v4()));
        let owner = AiHubNativeService::new(root.clone());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let result = owner
            .start_download(
                NativeDownloadSelection {
                    model_id: "voice".into(),
                    manifest_sha256: "aa".repeat(32),
                    backend: NativeBackend::Cpu,
                },
                tx,
            )
            .await;
        assert!(result.is_err());
        assert!(rx.try_recv().is_err());
        assert!(!root.exists());
    }
}
