//! Pure application presentation of durable native selection versus a live worker snapshot.
//! This module never reads files, starts workers or infers readiness from installed packages.

use crate::ai_runtime::{
    native_model::{NativeBackend, NativePhase},
    native_revisions::NativeSelectionManifestState,
    native_trust::NativeTrustVerdict,
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
    pub active_services: Vec<crate::ai_runtime::native_speech::NativeSpeechKind>,
    pub observed_phase: Option<NativePhase>,
    pub trust: NativeTrustVerdict,
}

pub struct NativeHubPresenter;

impl NativeHubPresenter {
    pub fn project(snapshot: NativeWorkflowSnapshot) -> NativeHubView {
        let trust = snapshot.trust;
        let inventory = NativeRevisionPresenter::project(snapshot.inventory);
        let invalid_selection = inventory
            .selections
            .iter()
            .any(|item| item.manifest_state != NativeSelectionManifestState::Present);
        let service_workers = snapshot.service_workers;
        let active = snapshot.active_revision;
        let worker = snapshot.worker.as_ref().map(NativeModelPresenter::project);
        let mut revisions = Vec::new();
        for item in inventory.models {
            for backend in item.backends {
                let selected = inventory.selections.iter().any(|selection| {
                    selection.model_id == item.model_id
                        && selection.manifest_sha256 == item.manifest_sha256
                        && selection.backend == backend
                });
                let is_selected_revision =
                    |choice: &crate::ai_runtime::native_revisions::NativeRevisionChoice| {
                        choice.model_id == item.model_id
                            && choice.backend == backend
                            && hex_digest(choice.manifest_sha256) == item.manifest_sha256
                    };
                let running = active.as_ref().is_some_and(is_selected_revision)
                    || service_workers
                        .iter()
                        .any(|session| is_selected_revision(&session.choice));
                let active_services = service_workers
                    .iter()
                    .filter(|session| is_selected_revision(&session.choice))
                    .map(|session| session.service)
                    .collect::<Vec<_>>();
                let observed_phase = if running {
                    worker
                        .as_ref()
                        .filter(|worker| {
                            worker.model_id == item.model_id && worker.backend == backend
                        })
                        .map(|worker| worker.phase)
                        .or_else(|| {
                            service_workers
                                .iter()
                                .find(|session| is_selected_revision(&session.choice))
                                .and_then(|session| {
                                    session.worker.as_ref().map(|worker| worker.phase)
                                })
                        })
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
                    active_services,
                    observed_phase,
                    trust: trust
                        .iter()
                        .find(|entry| {
                            entry.model_id == item.model_id
                                && hex_digest(entry.manifest_sha256) == item.manifest_sha256
                        })
                        .map_or(NativeTrustVerdict::Missing, |entry| entry.verdict.clone()),
                });
            }
        }
        NativeHubView {
            revisions,
            has_active_worker: active.is_some() || !service_workers.is_empty(),
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
        native_workflow::NativeRevisionTrust,
    };
    use uuid::Uuid;

    #[test]
    fn signed_approval_is_scoped_to_exact_model_and_revision() {
        let first = [0x11; 32];
        let second = [0x22; 32];
        let view = NativeHubPresenter::project(NativeWorkflowSnapshot {
            inventory: NativeRevisionOverview {
                revisions: [first, second]
                    .into_iter()
                    .map(|digest| NativeRevisionMetadata {
                        model_id: "voice".into(),
                        display_name: "Voice".into(),
                        package_version: "1".into(),
                        manifest_sha256: digest,
                        backends: vec![NativeBackend::Cpu],
                        speech_services: vec![],
                    })
                    .collect(),
                selections: vec![],
            },
            trust: vec![NativeRevisionTrust {
                model_id: "voice".into(),
                manifest_sha256: first,
                verdict: NativeTrustVerdict::Authorized {
                    publisher_id: "author".into(),
                },
            }],
            active_revision: None,
            worker: None,
            service_workers: Vec::new(),
        });
        assert!(matches!(
            view.revisions[0].trust,
            NativeTrustVerdict::Authorized { .. }
        ));
        assert_eq!(view.revisions[1].trust, NativeTrustVerdict::Missing);
        assert!(!view.has_active_worker);
    }

    #[test]
    fn a_new_selection_does_not_redirect_the_resident_worker() {
        let old = [1; 32];
        let new = [2; 32];
        let view = NativeHubPresenter::project(NativeWorkflowSnapshot {
            trust: Vec::new(),
            inventory: NativeRevisionOverview {
                revisions: [old, new]
                    .into_iter()
                    .map(|digest| NativeRevisionMetadata {
                        model_id: "voice".into(),
                        display_name: "Voice".into(),
                        package_version: "1".into(),
                        manifest_sha256: digest,
                        backends: vec![NativeBackend::Cpu],
                        speech_services: vec![],
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
            service_workers: Vec::new(),
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
            trust: Vec::new(),
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
            service_workers: Vec::new(),
        });
        assert!(view.invalid_selection);
        assert!(!view.has_active_worker);
        assert!(view.revisions.is_empty());
    }
}
