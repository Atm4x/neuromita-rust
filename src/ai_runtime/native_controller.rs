//! Application-owned selection and lifecycle for an admitted native executable.
//! A caller supplies a previously approved manifest digest; registration is not approval.

use std::{path::PathBuf, time::Duration};

use super::native_speech::NativeSpeechCapability;
use serde_json::Value;
use thiserror::Error;

use super::{
    native_model::{
        NativeBackend, NativeModelError, NativeModelProcess, NativeModelSnapshot, NativePhase,
    },
    native_registry::{NativeModelRegistration, NativeModelRegistry, NativeRegistryError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeModelSelection {
    pub model_id: String,
    pub approved_manifest_sha256: [u8; 32],
    pub backend: NativeBackend,
    pub request_timeout: Duration,
}

#[derive(Debug, Error)]
pub enum NativeControllerError {
    #[error("a native model session is already active; stop it before selecting another")]
    AlreadyActive,
    #[error("no native model session is active")]
    NotActive,
    #[error("the selected native model package was not discovered")]
    NotFound,
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Process(#[from] NativeModelError),
}

pub struct NativeModelController {
    packages_root: PathBuf,
    runtime_root: PathBuf,
    active: Option<NativeModelProcess>,
}

impl NativeModelController {
    pub fn new(packages_root: PathBuf, runtime_root: PathBuf) -> Self {
        Self {
            packages_root,
            runtime_root,
            active: None,
        }
    }

    pub fn discover(&self) -> Result<Vec<NativeModelRegistration>, NativeControllerError> {
        Ok(NativeModelRegistry::discover(&self.packages_root)?)
    }

    pub async fn activate(
        &mut self,
        selection: &NativeModelSelection,
    ) -> Result<NativeModelSnapshot, NativeControllerError> {
        if self.active.is_some() {
            return Err(NativeControllerError::AlreadyActive);
        }
        let registration = self
            .discover()?
            .into_iter()
            .find(|package| package.manifest().model_id == selection.model_id)
            .ok_or(NativeControllerError::NotFound)?;
        let launch = registration.prepare_launch(
            selection.approved_manifest_sha256,
            selection.backend,
            &self.runtime_root,
            selection.request_timeout,
        )?;
        let mut process = NativeModelProcess::start(launch).await?;
        process.load().await?;
        process.warmup().await?;
        let snapshot = process.refresh().await?;
        self.active = Some(process);
        Ok(snapshot)
    }

    pub async fn status(&mut self) -> Result<Option<NativeModelSnapshot>, NativeControllerError> {
        let Some(process) = self.active.as_mut() else {
            return Ok(None);
        };
        if process.snapshot().phase == NativePhase::Failed {
            return Ok(Some(process.snapshot()));
        }
        Ok(Some(process.refresh().await?))
    }

    pub fn speech_capabilities(&self) -> Option<&[NativeSpeechCapability]> {
        self.active
            .as_ref()
            .map(NativeModelProcess::speech_capabilities)
    }

    pub fn last_observed_snapshot(&self) -> Option<NativeModelSnapshot> {
        self.active.as_ref().map(NativeModelProcess::snapshot)
    }

    pub async fn infer(&mut self, payload: Value) -> Result<Value, NativeControllerError> {
        let process = self
            .active
            .as_mut()
            .ok_or(NativeControllerError::NotActive)?;
        Ok(process.infer(payload).await?)
    }

    pub async fn deactivate(&mut self) -> Result<(), NativeControllerError> {
        let mut process = self.active.take().ok_or(NativeControllerError::NotActive)?;
        match process.snapshot().phase {
            NativePhase::Loaded | NativePhase::Warm | NativePhase::Ready => {
                process.unload().await?;
            }
            NativePhase::Failed | NativePhase::Stopped => return Ok(()),
            NativePhase::Started => {}
        }
        process.shutdown().await?;
        Ok(())
    }
}
