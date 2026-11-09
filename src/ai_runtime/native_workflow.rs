//! Application-owned orchestration for installed native model revisions.
//! A persistent selection is not a live worker and a metadata view is not a content receipt.

use std::time::Duration;

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
};

#[derive(Debug, Error)]
pub enum NativeWorkflowError {
    #[error("a native model process is already active; stop it before activating another")]
    AlreadyActive,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeWorkflowSnapshot {
    pub inventory: NativeRevisionOverview,
    pub active_revision: Option<NativeRevisionChoice>,
    pub worker: Option<NativeModelSnapshot>,
}

struct ActiveNativeSession {
    choice: NativeRevisionChoice,
    controller: NativeModelController,
}

/// Exactly one active native worker belongs to this workflow. A user may change the
/// on-disk selected revision without redirecting the already activated process.
pub struct NativeModelWorkflow {
    revisions: NativeRevisionStore,
    active: Option<ActiveNativeSession>,
}

impl NativeModelWorkflow {
    pub fn new(revisions: NativeRevisionStore) -> Self {
        Self {
            revisions,
            active: None,
        }
    }

    pub fn snapshot(&self) -> Result<NativeWorkflowSnapshot, NativeWorkflowError> {
        Ok(NativeWorkflowSnapshot {
            inventory: self.revisions.overview()?,
            active_revision: self.active.as_ref().map(|session| session.choice.clone()),
            worker: self
                .active
                .as_ref()
                .and_then(|session| session.controller.last_observed_snapshot()),
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

    pub async fn activate_selected(
        &mut self,
        model_id: &str,
        deadline: Duration,
    ) -> Result<NativeModelSnapshot, NativeWorkflowError> {
        if self.active.is_some() {
            return Err(NativeWorkflowError::AlreadyActive);
        }
        let (mut controller, selection) = self
            .revisions
            .controller_for_selected(model_id, deadline)?
            .ok_or(NativeWorkflowError::NotSelected)?;
        let snapshot = controller.activate(&selection).await?;
        self.active = Some(ActiveNativeSession {
            choice: choice_from_selection(selection),
            controller,
        });
        Ok(snapshot)
    }

    pub async fn refresh_worker(
        &mut self,
    ) -> Result<Option<NativeModelSnapshot>, NativeWorkflowError> {
        match self.active.as_mut() {
            Some(session) => Ok(session.controller.status().await?),
            None => Ok(None),
        }
    }

    pub async fn infer(&mut self, payload: Value) -> Result<Value, NativeWorkflowError> {
        let session = self.active.as_mut().ok_or(NativeWorkflowError::NotActive)?;
        Ok(session.controller.infer(payload).await?)
    }

    pub async fn deactivate(&mut self) -> Result<(), NativeWorkflowError> {
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
