//! Pure application presentation of durable native selection versus a live worker snapshot.
//! This module never reads files, starts workers or infers readiness from installed packages.

use crate::ai_runtime::{
    native_model::{NativeBackend, NativePhase},
    native_revisions::NativeSelectionManifestState,
    native_workflow::NativeWorkflowSnapshot,
};

use super::{native_model::NativeModelPresenter, native_revisions::NativeRevisionPresenter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHubView {
    pub revisions: Vec<NativeHubRevision>,
    pub has_active_worker: bool,
    pub invalid_selection: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeHubRevision {
    pub model_id: String,
    pub display_name: String,
    pub package_version: String,
    pub manifest_sha256: String,
    pub backend: NativeBackend,
    pub selected: bool,
    pub active: bool,
    pub observed_phase: Option<NativePhase>,
}

pub struct NativeHubPresenter;

impl NativeHubPresenter {
    pub fn project(snapshot: NativeWorkflowSnapshot) -> NativeHubView {
        let inventory = NativeRevisionPresenter::project(snapshot.inventory);
        let invalid_selection = inventory
            .selections
            .iter()
            .any(|item| item.manifest_state != NativeSelectionManifestState::Present);
        let active = snapshot.active_revision;
        let active_digest = active
            .as_ref()
            .map(|choice| hex_digest(choice.manifest_sha256));
        let worker = snapshot.worker.as_ref().map(NativeModelPresenter::project);
        let mut revisions = Vec::new();
        for item in inventory.models {
            for backend in item.backends {
                let selected = inventory.selections.iter().any(|selection| {
                    selection.model_id == item.model_id
                        && selection.manifest_sha256 == item.manifest_sha256
                        && selection.backend == backend
                });
                let running = active.as_ref().is_some_and(|choice| {
                    choice.model_id == item.model_id
                        && choice.backend == backend
                        && active_digest.as_deref() == Some(&item.manifest_sha256)
                });
                let observed_phase = if running {
                    worker
                        .as_ref()
                        .filter(|worker| {
                            worker.model_id == item.model_id && worker.backend == backend
                        })
                        .map(|worker| worker.phase)
                } else {
                    None
                };
                revisions.push(NativeHubRevision {
                    model_id: item.model_id.clone(),
                    display_name: item.display_name.clone(),
                    package_version: item.package_version.clone(),
                    manifest_sha256: item.manifest_sha256.clone(),
                    backend,
                    selected,
                    active: running,
                    observed_phase,
                });
            }
        }
        NativeHubView {
            revisions,
            has_active_worker: active.is_some(),
            invalid_selection,
        }
    }
}

fn hex_digest(value: [u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::{
        native_model::NativeModelSnapshot,
        native_revisions::{
            NativeRevisionChoice, NativeRevisionMetadata, NativeRevisionOverview,
            NativeSelectionMetadata,
        },
    };
    use uuid::Uuid;

    #[test]
    fn a_new_selection_does_not_redirect_the_resident_worker() {
        let old = [1; 32];
        let new = [2; 32];
        let view = NativeHubPresenter::project(NativeWorkflowSnapshot {
            inventory: NativeRevisionOverview {
                revisions: [old, new]
                    .into_iter()
                    .map(|digest| NativeRevisionMetadata {
                        model_id: "voice".into(),
                        display_name: "Voice".into(),
                        package_version: "1".into(),
                        manifest_sha256: digest,
                        backends: vec![NativeBackend::Cpu],
                    })
                    .collect(),
                selections: vec![NativeSelectionMetadata {
                    choice: NativeRevisionChoice {
                        model_id: "voice".into(),
                        manifest_sha256: new,
                        backend: NativeBackend::Cpu,
                    },
                    manifest_state: NativeSelectionManifestState::Present,
                }],
            },
            active_revision: Some(NativeRevisionChoice {
                model_id: "voice".into(),
                manifest_sha256: old,
                backend: NativeBackend::Cpu,
            }),
            worker: Some(NativeModelSnapshot {
                model_id: "voice".into(),
                backend: NativeBackend::Cpu,
                session_id: Uuid::new_v4(),
                phase: NativePhase::Ready,
                last_failure: None,
            }),
        });
        assert!(view.has_active_worker);
        assert!(!view.invalid_selection);
        assert!(view.revisions[0].active);
        assert!(!view.revisions[0].selected);
        assert_eq!(view.revisions[0].observed_phase, Some(NativePhase::Ready));
        assert!(view.revisions[1].selected);
        assert!(!view.revisions[1].active);
        assert_eq!(view.revisions[1].observed_phase, None);
    }

    #[test]
    fn missing_selected_version_is_not_promoted_to_ready() {
        let view = NativeHubPresenter::project(NativeWorkflowSnapshot {
            inventory: NativeRevisionOverview {
                revisions: Vec::new(),
                selections: vec![NativeSelectionMetadata {
                    choice: NativeRevisionChoice {
                        model_id: "voice".into(),
                        manifest_sha256: [7; 32],
                        backend: NativeBackend::Cuda,
                    },
                    manifest_state: NativeSelectionManifestState::Missing,
                }],
            },
            active_revision: None,
            worker: None,
        });
        assert!(view.invalid_selection);
        assert!(!view.has_active_worker);
        assert!(view.revisions.is_empty());
    }
}
