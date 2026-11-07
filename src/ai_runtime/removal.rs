//! Read-only, conservative preflight for removing immutable AI package layers.
//!
//! This assessment is not authority to delete a revision: it cannot prove that
//! another process or a subsequent worker activation will not acquire it.
//! Physical removal requires an exclusive application-owned lifecycle boundary.

use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

use super::{
    AiService,
    backend::{BackendInstaller, LockedBackend},
    composition::ServiceSelection,
    layer::LayerError,
    overlay::{LockedOverlay, OverlayInstaller},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorkerLayers {
    pub generation: Uuid,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayerUsageError {
    #[error("AI worker or activation is busy; layer ownership cannot be determined")]
    Busy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalBlocker {
    SelectedBy(AiService),
    ActiveGeneration(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalAssessment {
    pub path: PathBuf,
    pub blockers: Vec<RemovalBlocker>,
}

impl RemovalAssessment {
    /// No blockers in this snapshot does not authorize filesystem deletion.
    pub fn has_observed_references(&self) -> bool {
        !self.blockers.is_empty()
    }
}

pub fn assess_backend(
    installer: &BackendInstaller,
    backend: &LockedBackend,
    configured: &[ServiceSelection],
    running: &[ActiveWorkerLayers],
) -> Result<RemovalAssessment, LayerError> {
    let path = installer.installed_path(backend)?;
    assess(&path, configured, running, |selection| {
        for other in &selection.backends {
            if installer.installed_path(other)? == path {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

pub fn assess_overlay(
    installer: &OverlayInstaller,
    overlay: &LockedOverlay,
    configured: &[ServiceSelection],
    running: &[ActiveWorkerLayers],
) -> Result<RemovalAssessment, LayerError> {
    let path = installer.installed_path(overlay)?;
    assess(&path, configured, running, |selection| {
        for other in &selection.overlays {
            if installer.installed_path(other)? == path {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

fn assess(
    path: &std::path::Path,
    configured: &[ServiceSelection],
    running: &[ActiveWorkerLayers],
    selected: impl Fn(&ServiceSelection) -> Result<bool, LayerError>,
) -> Result<RemovalAssessment, LayerError> {
    if !path.is_absolute() {
        return Err(LayerError::Invalid("layer path must be absolute".into()));
    }
    let mut blockers = Vec::new();
    for selection in configured {
        if selected(selection)? {
            let blocker = RemovalBlocker::SelectedBy(selection.service);
            if !blockers.contains(&blocker) {
                blockers.push(blocker);
            }
        }
    }
    for worker in running {
        if worker.paths.iter().any(|candidate| candidate == path) {
            let blocker = RemovalBlocker::ActiveGeneration(worker.generation);
            if !blockers.contains(&blocker) {
                blockers.push(blocker);
            }
        }
    }
    Ok(RemovalAssessment {
        path: path.to_owned(),
        blockers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::backend::BackendKind;
    use std::time::Duration;

    fn backend() -> LockedBackend {
        LockedBackend {
            kind: BackendKind::RemoteApi,
            revision: "shared1".into(),
            python_version: "3.13.5".into(),
            packages: vec![],
        }
    }

    fn overlay(id: &str) -> LockedOverlay {
        LockedOverlay {
            plugin_id: id.into(),
            revision: "overlay1".into(),
            python_version: "3.13.5".into(),
            packages: vec![],
            actions: vec![],
        }
    }

    fn installers() -> (BackendInstaller, OverlayInstaller) {
        let root = std::env::temp_dir().join(format!("nm-removal-{}", Uuid::new_v4()));
        let backend = BackendInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            backends_directory: root.join("backends"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        let overlay = OverlayInstaller {
            uv_executable: root.join("uv"),
            python_executable: root.join("python"),
            overlays_directory: root.join("overlays"),
            cache_directory: root.join("cache"),
            timeout: Duration::from_secs(5),
        };
        (backend, overlay)
    }

    #[test]
    fn shared_backend_remains_referenced_after_one_overlay_is_deselected() {
        let (backend_installer, overlay_installer) = installers();
        let shared = backend();
        let one = overlay("tts_one");
        let two = overlay("tts_two");
        let configured = vec![ServiceSelection {
            service: AiService::Asr,
            backends: vec![shared.clone()],
            overlays: vec![two.clone()],
        }];
        let stale_worker = ActiveWorkerLayers {
            generation: Uuid::new_v4(),
            paths: vec![overlay_installer.installed_path(&one).unwrap()],
        };
        let backend_assessment =
            assess_backend(&backend_installer, &shared, &configured, &[]).unwrap();
        assert_eq!(
            backend_assessment.blockers,
            vec![RemovalBlocker::SelectedBy(AiService::Asr)]
        );
        let removed_selection = assess_overlay(
            &overlay_installer,
            &one,
            &configured,
            std::slice::from_ref(&stale_worker),
        )
        .unwrap();
        assert_eq!(
            removed_selection.blockers,
            vec![RemovalBlocker::ActiveGeneration(stale_worker.generation)]
        );
        let unused = assess_overlay(&overlay_installer, &one, &configured, &[]).unwrap();
        assert!(!unused.has_observed_references());
        let remaining = assess_overlay(&overlay_installer, &two, &configured, &[]).unwrap();
        assert!(remaining.has_observed_references());
    }

    #[test]
    fn a_shared_worker_blocks_multiple_revisions_and_duplicate_paths_are_deduplicated() {
        let (backend_installer, overlay_installer) = installers();
        let shared = backend();
        let layer = overlay("tts_one");
        let backend_path = backend_installer.installed_path(&shared).unwrap();
        let overlay_path = overlay_installer.installed_path(&layer).unwrap();
        let generation = Uuid::new_v4();
        let active = [ActiveWorkerLayers {
            generation,
            paths: vec![backend_path.clone(), overlay_path.clone(), backend_path],
        }];
        assert_eq!(
            assess_backend(&backend_installer, &shared, &[], &active)
                .unwrap()
                .blockers,
            vec![RemovalBlocker::ActiveGeneration(generation)]
        );
        assert_eq!(
            assess_overlay(&overlay_installer, &layer, &[], &active)
                .unwrap()
                .blockers,
            vec![RemovalBlocker::ActiveGeneration(generation)]
        );
    }

    #[test]
    fn conflicting_receipt_identity_at_same_path_still_blocks_removal() {
        let (backend_installer, overlay_installer) = installers();
        let shared = backend();
        let one = overlay("tts_one");
        let mut changed_backend = shared.clone();
        changed_backend.python_version = "3.12.9".into();
        let mut changed_overlay = one.clone();
        changed_overlay.python_version = "3.12.9".into();
        let configured = [ServiceSelection {
            service: AiService::Tts,
            backends: vec![changed_backend],
            overlays: vec![changed_overlay],
        }];
        assert_eq!(
            assess_backend(&backend_installer, &shared, &configured, &[])
                .unwrap()
                .blockers,
            vec![RemovalBlocker::SelectedBy(AiService::Tts)]
        );
        assert_eq!(
            assess_overlay(&overlay_installer, &one, &configured, &[])
                .unwrap()
                .blockers,
            vec![RemovalBlocker::SelectedBy(AiService::Tts)]
        );
    }

    #[test]
    fn invalid_layer_revision_rejected_before_assessment() {
        let (installer, _) = installers();
        let mut plan = backend();
        plan.revision = "../outside".into();
        assert!(matches!(
            assess_backend(&installer, &plan, &[], &[]),
            Err(LayerError::Invalid(_))
        ));
    }
}
