//! Application-owned orchestration for installed native model revisions.
//! A persistent selection is not a live worker and a metadata view is not a content receipt.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::{Mutex, watch};

use serde_json::Value;
use thiserror::Error;

use super::{
    native_approval::VerifiedNativeRelease,
    native_controller::{NativeControllerError, NativeModelController, NativeModelSelection},
    native_model::{NativeBackend, NativeModelSnapshot},
    native_registry::{NativeRegistryError, parse_sha256},
    native_revisions::{
        NativeRevisionChoice, NativeRevisionError, NativeRevisionOverview, NativeRevisionStore,
    },
    native_speech::{self, NativeSpeechError, NativeSpeechKind},
    native_trust::{NativeReleaseTrust, NativeTrustError, NativeTrustVerdict},
};

#[derive(Debug, Error)]
pub enum NativeWorkflowError {
    #[error("a native model process is already active; stop it before activating another")]
    AlreadyActive,
    #[error("native model advertises multiple services; select the service explicitly")]
    AmbiguousService,
    #[error("native model does not implement the requested service")]
    UnsupportedService,
    #[error("no selected native model revision exists")]
    NotSelected,
    #[error("no native model process is active")]
    NotActive,
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Revisions(#[from] NativeRevisionError),
    #[error(transparent)]
    Controller(#[from] NativeControllerError),
    #[error(transparent)]
    Trust(#[from] NativeTrustError),
    #[error(transparent)]
    Speech(#[from] NativeSpeechError),
    #[error("active native model identity differs from the selected speech model")]
    ModelMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeWorkflowSnapshot {
    pub inventory: NativeRevisionOverview,
    pub trust: Vec<NativeRevisionTrust>,
    pub active_revision: Option<NativeRevisionChoice>,
    pub worker: Option<NativeModelSnapshot>,
    pub service_workers: Vec<NativeServiceWorkerSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeServiceWorkerSnapshot {
    pub service: NativeSpeechKind,
    pub choice: NativeRevisionChoice,
    pub worker: Option<NativeModelSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRevisionTrust {
    pub model_id: String,
    pub manifest_sha256: [u8; 32],
    pub verdict: NativeTrustVerdict,
}

struct ActiveNativeSession {
    choice: NativeRevisionChoice,
    controller: NativeModelController,
}

/// The process and correlated IPC channel belong to one service. Unlike the
/// metadata workflow, its async lock is never shared with another service.
pub(crate) struct NativeSpeechSession {
    service: NativeSpeechKind,
    choice: NativeRevisionChoice,
    controller: Mutex<NativeModelController>,
    observed: watch::Sender<Option<NativeModelSnapshot>>,
}

/// Preparation is synchronous and cannot spawn. Startup runs outside the
/// workflow catalog lock, behind the requested service's exclusive gate.
pub(crate) struct PreparedSpeechWorker {
    service: NativeSpeechKind,
    controller: NativeModelController,
    selection: NativeModelSelection,
}

impl PreparedSpeechWorker {
    pub(crate) fn choice(&self) -> NativeRevisionChoice {
        choice_from_selection(self.selection.clone())
    }

    pub(crate) async fn start(
        mut self,
    ) -> Result<(Arc<NativeSpeechSession>, NativeModelSnapshot), NativeWorkflowError> {
        let snapshot = self.controller.activate(&self.selection).await?;
        let (observed, _) = watch::channel(Some(snapshot.clone()));
        Ok((
            Arc::new(NativeSpeechSession {
                service: self.service,
                choice: choice_from_selection(self.selection),
                controller: Mutex::new(self.controller),
                observed,
            }),
            snapshot,
        ))
    }
}

impl NativeSpeechSession {
    fn snapshot(&self) -> NativeServiceWorkerSnapshot {
        NativeServiceWorkerSnapshot {
            service: self.service,
            choice: self.choice.clone(),
            worker: self.observed.borrow().clone(),
        }
    }

    pub(crate) async fn refresh(&self) -> Result<Option<NativeModelSnapshot>, NativeWorkflowError> {
        let mut controller = self.controller.lock().await;
        let result = controller.status().await;
        self.observed
            .send_replace(controller.last_observed_snapshot());
        Ok(result?)
    }

    pub(crate) async fn synthesize(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        text: &str,
        voice: &str,
        language: &str,
        volume_percent: u8,
    ) -> Result<crate::voice::AudioArtifact, NativeWorkflowError> {
        self.verify(model_id, manifest_sha256)?;
        let mut controller = self.controller.lock().await;
        let caps = controller
            .speech_capabilities()
            .ok_or(NativeWorkflowError::NotActive)?
            .to_vec();
        let payload = native_speech::encode_tts(&caps, text, voice, language, volume_percent)?;
        let result = controller.infer(payload).await;
        self.observed
            .send_replace(controller.last_observed_snapshot());
        Ok(native_speech::decode_tts(&caps, language, result?)?)
    }

    pub(crate) async fn transcribe(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        wav: &[u8],
        language: &str,
    ) -> Result<crate::ai_runtime::speech::Transcript, NativeWorkflowError> {
        self.verify(model_id, manifest_sha256)?;
        let mut controller = self.controller.lock().await;
        let caps = controller
            .speech_capabilities()
            .ok_or(NativeWorkflowError::NotActive)?
            .to_vec();
        native_speech::capability(&caps, NativeSpeechKind::Asr, language)?;
        let payload = native_speech::encode_asr(&caps, wav, language)?;
        let result = controller.infer(payload).await;
        self.observed
            .send_replace(controller.last_observed_snapshot());
        let transcript = native_speech::decode_asr(result?)?;
        if let Some(language) = &transcript.language {
            native_speech::capability(&caps, NativeSpeechKind::Asr, language)?;
        }
        Ok(transcript)
    }

    fn verify(&self, model_id: &str, manifest_sha256: &str) -> Result<(), NativeWorkflowError> {
        if self.choice.model_id != model_id
            || self.choice.manifest_sha256 != parse_sha256(manifest_sha256)?
        {
            return Err(NativeWorkflowError::ModelMismatch);
        }
        Ok(())
    }

    pub(crate) async fn deactivate(&self) -> Result<(), NativeWorkflowError> {
        let mut controller = self.controller.lock().await;
        let result = controller.deactivate().await;
        self.observed
            .send_replace(controller.last_observed_snapshot());
        Ok(result?)
    }
}

/// Exactly one active native worker belongs to this workflow. A user may change the
/// on-disk selected revision without redirecting the already activated process.
pub struct NativeModelWorkflow {
    revisions: NativeRevisionStore,
    trust: Option<NativeReleaseTrust>,
    active: Option<ActiveNativeSession>,
    speech_workers: HashMap<NativeSpeechKind, Arc<NativeSpeechSession>>,
}

impl NativeModelWorkflow {
    pub fn new(revisions: NativeRevisionStore) -> Self {
        Self {
            revisions,
            trust: None,
            active: None,
            speech_workers: HashMap::new(),
        }
    }

    /// Product workflows must have this independently provisioned publisher trust source.
    /// An absent policy never permits unsigned selection or worker startup.
    pub fn with_trust(revisions: NativeRevisionStore, trust: NativeReleaseTrust) -> Self {
        Self {
            revisions,
            trust: Some(trust),
            active: None,
            speech_workers: HashMap::new(),
        }
    }

    pub fn snapshot(&self) -> Result<NativeWorkflowSnapshot, NativeWorkflowError> {
        let inventory = self.revisions.overview()?;
        let trust = inventory
            .revisions
            .iter()
            .map(|revision| NativeRevisionTrust {
                model_id: revision.model_id.clone(),
                manifest_sha256: revision.manifest_sha256,
                verdict: self
                    .trust
                    .as_ref()
                    .map_or(NativeTrustVerdict::Missing, |policy| {
                        policy.verdict(&revision.model_id, &revision.manifest_sha256)
                    }),
            })
            .collect();
        let mut service_workers = self
            .speech_workers
            .iter()
            .map(|(_, session)| session.snapshot())
            .collect::<Vec<_>>();
        service_workers.sort_by_key(|worker| match worker.service {
            NativeSpeechKind::Tts => 0,
            NativeSpeechKind::Asr => 1,
        });
        let generic = self.active.as_ref();
        let single_speech = if generic.is_none() && self.speech_workers.len() == 1 {
            self.speech_workers.values().next()
        } else {
            None
        };
        Ok(NativeWorkflowSnapshot {
            inventory,
            trust,
            active_revision: generic
                .map(|session| session.choice.clone())
                .or_else(|| single_speech.map(|session| session.choice.clone())),
            worker: generic
                .and_then(|session| session.controller.last_observed_snapshot())
                .or_else(|| single_speech.and_then(|session| session.observed.borrow().clone())),
            service_workers,
        })
    }

    /// Prefer this path for production-approved releases. The approval must
    /// have been verified against a publisher key pinned outside model data.
    pub fn select_verified(
        &self,
        approved: &VerifiedNativeRelease,
        backend: NativeBackend,
    ) -> Result<(), NativeWorkflowError> {
        self.revisions.select(&NativeRevisionChoice {
            model_id: approved.model_id().to_owned(),
            manifest_sha256: approved.manifest_sha256(),
            backend,
        })?;
        Ok(())
    }

    /// Product-facing entrypoint: the untrusted GUI digest is only a lookup key.
    /// A release must be signed by a currently enrolled independent publisher.
    pub fn select_trusted(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        backend: NativeBackend,
    ) -> Result<(), NativeWorkflowError> {
        let digest = parse_sha256(manifest_sha256)?;
        let policy = self.trust.as_ref().ok_or(NativeTrustError::NotAuthorized)?;
        let approved = policy.verify(model_id, &digest)?;
        self.select_verified(&approved, backend)
    }

    /// The digest must come from a separately approved, already published package.
    /// A string passed through the GUI is never itself a trust decision.
    pub fn select(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        backend: NativeBackend,
    ) -> Result<(), NativeWorkflowError> {
        let choice = NativeRevisionChoice {
            model_id: model_id.to_owned(),
            manifest_sha256: parse_sha256(manifest_sha256)?,
            backend,
        };
        self.revisions.select(&choice)?;
        Ok(())
    }

    /// Compatibility entry point for older generic packages and single-service
    /// speech packages. A dual-capability package requires an explicit service.
    pub fn selected_speech_services(
        &self,
        model_id: &str,
    ) -> Result<Vec<NativeSpeechKind>, NativeWorkflowError> {
        let (controller, selection) = self.prepared_selected(model_id, Duration::from_secs(30))?;
        let registered = controller
            .discover()?
            .into_iter()
            .find(|entry| entry.manifest().model_id == selection.model_id)
            .ok_or(NativeControllerError::NotFound)?;
        Ok(registered
            .manifest()
            .speech_capabilities
            .iter()
            .map(|c| c.kind)
            .collect())
    }

    pub fn selected_choice(
        &self,
        model_id: &str,
    ) -> Result<NativeRevisionChoice, NativeWorkflowError> {
        let (_, selected) = self.prepared_selected(model_id, Duration::from_secs(30))?;
        Ok(choice_from_selection(selected))
    }

    pub async fn activate_selected(
        &mut self,
        model_id: &str,
        deadline: Duration,
    ) -> Result<NativeModelSnapshot, NativeWorkflowError> {
        let (controller, selection) = self.prepared_selected(model_id, deadline)?;
        let registration = controller
            .discover()?
            .into_iter()
            .find(|entry| entry.manifest().model_id == selection.model_id)
            .ok_or(NativeControllerError::NotFound)?;
        match registration.manifest().speech_capabilities.as_slice() {
            [] => self.activate_generic(controller, selection).await,
            [capability] => {
                self.activate_speech_session(capability.kind, controller, selection)
                    .await
            }
            _ => Err(NativeWorkflowError::AmbiguousService),
        }
    }

    /// Only the service owner may activate a prepared worker. Its transition
    /// permit stays alive from preparation through process startup and commit.
    pub(crate) fn prepare_for_service(
        &self,
        service: NativeSpeechKind,
        model_id: &str,
        deadline: Duration,
    ) -> Result<PreparedSpeechWorker, NativeWorkflowError> {
        if self.speech_workers.contains_key(&service) {
            return Err(NativeWorkflowError::AlreadyActive);
        }
        let (controller, selection) = self.prepared_selected(model_id, deadline)?;
        let registration = controller
            .discover()?
            .into_iter()
            .find(|entry| entry.manifest().model_id == selection.model_id)
            .ok_or(NativeControllerError::NotFound)?;
        if !registration
            .manifest()
            .speech_capabilities
            .iter()
            .any(|capability| capability.kind == service)
        {
            return Err(NativeWorkflowError::UnsupportedService);
        }
        Ok(PreparedSpeechWorker {
            service,
            controller,
            selection,
        })
    }

    pub(crate) fn install_active_speech(
        &mut self,
        session: Arc<NativeSpeechSession>,
    ) -> Result<(), NativeWorkflowError> {
        if self.speech_workers.contains_key(&session.service) {
            return Err(NativeWorkflowError::AlreadyActive);
        }
        self.speech_workers.insert(session.service, session);
        Ok(())
    }

    pub async fn activate_for_service(
        &mut self,
        service: NativeSpeechKind,
        model_id: &str,
        deadline: Duration,
    ) -> Result<NativeModelSnapshot, NativeWorkflowError> {
        let prepared = self.prepare_for_service(service, model_id, deadline)?;
        let (session, snapshot) = prepared.start().await?;
        self.install_active_speech(session)?;
        Ok(snapshot)
    }

    fn prepared_selected(
        &self,
        model_id: &str,
        deadline: Duration,
    ) -> Result<(NativeModelController, NativeModelSelection), NativeWorkflowError> {
        if let Some(policy) = &self.trust {
            let choice = self
                .revisions
                .selection_hint(model_id)?
                .ok_or(NativeWorkflowError::NotSelected)?;
            policy.verify(&choice.model_id, &choice.manifest_sha256)?;
        }
        let (controller, selection) = self
            .revisions
            .controller_for_selected(model_id, deadline)?
            .ok_or(NativeWorkflowError::NotSelected)?;
        if let Some(policy) = &self.trust {
            policy.verify(&selection.model_id, &selection.approved_manifest_sha256)?;
        }
        Ok((controller, selection))
    }

    async fn activate_generic(
        &mut self,
        mut controller: NativeModelController,
        selection: NativeModelSelection,
    ) -> Result<NativeModelSnapshot, NativeWorkflowError> {
        if self.active.is_some() {
            return Err(NativeWorkflowError::AlreadyActive);
        }
        let snapshot = controller.activate(&selection).await?;
        self.active = Some(ActiveNativeSession {
            choice: choice_from_selection(selection),
            controller,
        });
        Ok(snapshot)
    }

    async fn activate_speech_session(
        &mut self,
        service: NativeSpeechKind,
        controller: NativeModelController,
        selection: NativeModelSelection,
    ) -> Result<NativeModelSnapshot, NativeWorkflowError> {
        if self.speech_workers.contains_key(&service) {
            return Err(NativeWorkflowError::AlreadyActive);
        }
        let (session, snapshot) = PreparedSpeechWorker {
            service,
            controller,
            selection,
        }
        .start()
        .await?;
        self.install_active_speech(session)?;
        Ok(snapshot)
    }

    pub(crate) fn speech_session(
        &self,
        service: NativeSpeechKind,
    ) -> Result<Arc<NativeSpeechSession>, NativeWorkflowError> {
        self.speech_workers
            .get(&service)
            .cloned()
            .ok_or(NativeWorkflowError::NotActive)
    }

    pub async fn refresh_worker(
        &mut self,
    ) -> Result<Option<NativeModelSnapshot>, NativeWorkflowError> {
        if let Some(session) = self.active.as_mut() {
            return Ok(session.controller.status().await?);
        }
        match self.speech_workers.len() {
            0 => Ok(None),
            1 => {
                let session = self
                    .speech_workers
                    .values()
                    .next()
                    .expect("single worker")
                    .clone();
                session.refresh().await
            }
            _ => Err(NativeWorkflowError::AmbiguousService),
        }
    }

    pub async fn refresh_service(
        &self,
        service: NativeSpeechKind,
    ) -> Result<Option<NativeModelSnapshot>, NativeWorkflowError> {
        self.speech_session(service)?.refresh().await
    }

    pub async fn synthesize_speech(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        text: &str,
        voice: &str,
        language: &str,
        volume_percent: u8,
    ) -> Result<crate::voice::AudioArtifact, NativeWorkflowError> {
        self.speech_session(NativeSpeechKind::Tts)?
            .synthesize(
                model_id,
                manifest_sha256,
                text,
                voice,
                language,
                volume_percent,
            )
            .await
    }

    pub async fn transcribe_speech(
        &self,
        model_id: &str,
        manifest_sha256: &str,
        wav: &[u8],
        language: &str,
    ) -> Result<crate::ai_runtime::speech::Transcript, NativeWorkflowError> {
        self.speech_session(NativeSpeechKind::Asr)?
            .transcribe(model_id, manifest_sha256, wav, language)
            .await
    }

    pub async fn infer(&mut self, payload: Value) -> Result<Value, NativeWorkflowError> {
        let session = self.active.as_mut().ok_or(NativeWorkflowError::NotActive)?;
        Ok(session.controller.infer(payload).await?)
    }

    pub(crate) async fn refresh_generic_worker(&mut self) -> Result<(), NativeWorkflowError> {
        if let Some(session) = self.active.as_mut() {
            session.controller.status().await?;
        }
        Ok(())
    }

    pub async fn refresh_all_workers(&mut self) -> Result<(), NativeWorkflowError> {
        self.refresh_generic_worker().await?;
        for session in self.speech_workers.values() {
            session.refresh().await?;
        }
        Ok(())
    }

    pub(crate) fn unpublish_stopped_speech(
        &mut self,
        service: NativeSpeechKind,
        stopped: &Arc<NativeSpeechSession>,
    ) -> Result<(), NativeWorkflowError> {
        let active = self
            .speech_workers
            .get(&service)
            .ok_or(NativeWorkflowError::NotActive)?;
        if !Arc::ptr_eq(active, stopped) {
            return Err(NativeWorkflowError::ModelMismatch);
        }
        self.speech_workers.remove(&service);
        Ok(())
    }

    pub async fn deactivate_for_service(
        &mut self,
        service: NativeSpeechKind,
    ) -> Result<(), NativeWorkflowError> {
        let session = self.speech_session(service)?;
        session.deactivate().await?;
        self.unpublish_stopped_speech(service, &session)
    }

    pub async fn deactivate_all(&mut self) -> Result<(), NativeWorkflowError> {
        let mut first_error = None;
        for service in [NativeSpeechKind::Tts, NativeSpeechKind::Asr] {
            if self.speech_workers.contains_key(&service) {
                if let Err(error) = self.deactivate_for_service(service).await {
                    first_error.get_or_insert(error);
                }
            }
        }
        if self.active.is_some() {
            if let Err(error) = self.deactivate().await {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub async fn deactivate(&mut self) -> Result<(), NativeWorkflowError> {
        if self.active.is_none() && self.speech_workers.len() == 1 {
            let service = *self.speech_workers.keys().next().expect("single worker");
            return self.deactivate_for_service(service).await;
        }
        if self.active.is_none() && self.speech_workers.len() > 1 {
            return Err(NativeWorkflowError::AmbiguousService);
        }
        let mut session = self.active.take().ok_or(NativeWorkflowError::NotActive)?;
        session.controller.deactivate().await?;
        Ok(())
    }
}

fn choice_from_selection(selection: NativeModelSelection) -> NativeRevisionChoice {
    NativeRevisionChoice {
        model_id: selection.model_id,
        manifest_sha256: selection.approved_manifest_sha256,
        backend: selection.backend,
    }
}
