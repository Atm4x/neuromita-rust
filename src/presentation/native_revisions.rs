//! Lightweight native model selection and revision view. Package readiness belongs to Core,
//! and inference readiness belongs exclusively to the running worker.

use crate::ai_runtime::{
    native_model::NativeBackend,
    native_revisions::{
        NativeRevisionError, NativeRevisionOverview, NativeRevisionStore,
        NativeSelectionManifestState,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRevisionView {
    pub models: Vec<NativePackageView>,
    pub selections: Vec<NativeSelectionView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArtifactVerification {
    NotChecked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeWorkerObservation {
    NotObserved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePackageView {
    pub model_id: String,
    pub display_name: String,
    pub package_version: String,
    pub manifest_sha256: String,
    pub backends: Vec<NativeBackend>,
    pub artifact_verification: NativeArtifactVerification,
    pub worker_observation: NativeWorkerObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSelectionView {
    pub model_id: String,
    pub manifest_sha256: String,
    pub backend: NativeBackend,
    pub manifest_state: NativeSelectionManifestState,
    pub artifact_verification: NativeArtifactVerification,
    pub worker_observation: NativeWorkerObservation,
}

pub struct NativeRevisionPresenter;

impl NativeRevisionPresenter {
    pub fn read(store: &NativeRevisionStore) -> Result<NativeRevisionView, NativeRevisionError> {
        Ok(Self::project(store.overview()?))
    }

    pub fn project(overview: NativeRevisionOverview) -> NativeRevisionView {
        NativeRevisionView {
            models: overview
                .revisions
                .into_iter()
                .map(|revision| NativePackageView {
                    model_id: revision.model_id,
                    display_name: revision.display_name,
                    package_version: revision.package_version,
                    manifest_sha256: digest_hex(revision.manifest_sha256),
                    backends: revision.backends,
                    artifact_verification: NativeArtifactVerification::NotChecked,
                    worker_observation: NativeWorkerObservation::NotObserved,
                })
                .collect(),
            selections: overview
                .selections
                .into_iter()
                .map(|selection| NativeSelectionView {
                    model_id: selection.choice.model_id,
                    manifest_sha256: digest_hex(selection.choice.manifest_sha256),
                    backend: selection.choice.backend,
                    manifest_state: selection.manifest_state,
                    artifact_verification: NativeArtifactVerification::NotChecked,
                    worker_observation: NativeWorkerObservation::NotObserved,
                })
                .collect(),
        }
    }
}

fn digest_hex(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
