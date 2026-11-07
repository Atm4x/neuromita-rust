//! Read-only, conservative preflight for removing immutable AI package layers.
//!
//! This assessment is not authority to delete a revision: it cannot prove that
//! another process or a subsequent worker activation will not acquire it.
//! Physical removal requires an exclusive application-owned lifecycle boundary.

use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

use serde::de::DeserializeOwned;
use thiserror::Error;
use uuid::Uuid;

use super::{
    AiService,
    backend::{BackendInstaller, LockedBackend},
    composition::ServiceSelection,
    layer::{LayerError, reject_unsafe_layer_receipt},
    overlay::{LockedOverlay, OverlayInstaller},
    revision_lease::RevisionLease,
};

use crate::settings::SettingsInstanceLease;

#[allow(dead_code)]
pub(crate) struct LeasedRemovalInspection<'a> {
    pub(crate) assessment: RemovalAssessment,
    _root_owner: &'a SettingsInstanceLease,
    _revision_owner: RevisionLease,
}

/// An exclusive, receipt-verified, read-only inspection.
///
/// The caller must already own the portable root. The revision lock is acquired
/// only after that root lease, so cooperating Python imports cannot start while
/// this inspection is held. In-process setting edits and unknown repository
/// implementations can still change references: this is NOT a delete permit.
#[allow(dead_code)]
pub(crate) fn inspect_backend_with_root<'a>(
    root_owner: &'a SettingsInstanceLease,
    data_root: &Path,
    installer: &BackendInstaller,
    backend: &LockedBackend,
    configured: &[ServiceSelection],
    running: &[ActiveWorkerLayers],
) -> Result<LeasedRemovalInspection<'a>, LayerError> {
    let path = installer.installed_path(backend)?;
    let revision_owner = lock_owned_revision(root_owner, data_root, &path, "backends")?;
    check_receipt(&path, ".neuromita-backend.json", backend)?;
    let assessment = assess_backend(installer, backend, configured, running)?;
    Ok(LeasedRemovalInspection {
        assessment,
        _root_owner: root_owner,
        _revision_owner: revision_owner,
    })
}

#[allow(dead_code)]
pub(crate) fn inspect_overlay_with_root<'a>(
    root_owner: &'a SettingsInstanceLease,
    data_root: &Path,
    installer: &OverlayInstaller,
    overlay: &LockedOverlay,
    configured: &[ServiceSelection],
    running: &[ActiveWorkerLayers],
) -> Result<LeasedRemovalInspection<'a>, LayerError> {
    let path = installer.installed_path(overlay)?;
    let revision_owner = lock_owned_revision(root_owner, data_root, &path, "overlays")?;
    check_receipt(&path, ".neuromita-overlay.json", overlay)?;
    let assessment = assess_overlay(installer, overlay, configured, running)?;
    Ok(LeasedRemovalInspection {
        assessment,
        _root_owner: root_owner,
        _revision_owner: revision_owner,
    })
}

fn lock_owned_revision(
    root_owner: &SettingsInstanceLease,
    data_root: &Path,
    revision: &Path,
    category: &str,
) -> Result<RevisionLease, LayerError> {
    let canonical_root = data_root.canonicalize()?;
    if !root_owner.matches_root(&canonical_root)? {
        return Err(LayerError::Invalid(
            "removal inspection requires the matching portable data root owner".into(),
        ));
    }
    let category_root = canonical_root.join("AIData").join(category);
    if revision.parent().and_then(Path::parent) != Some(category_root.as_path()) {
        return Err(LayerError::Invalid(
            "revision must be located in the exact managed backend or overlay root".into(),
        ));
    }
    let mut cursor = revision;
    while cursor != canonical_root {
        let metadata = std::fs::symlink_metadata(cursor)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(LayerError::Invalid(
                "managed revision and every parent must be real directories".into(),
            ));
        }
        cursor = cursor.parent().ok_or_else(|| {
            LayerError::Invalid("revision path escapes its portable data root".into())
        })?;
    }
    let lock = RevisionLease::exclusive(revision)?;
    if !revision.is_dir() || revision.is_symlink() {
        return Err(LayerError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "installed revision disappeared during exclusive inspection",
        )));
    }
    Ok(lock)
}

fn check_receipt<T: DeserializeOwned + PartialEq>(
    revision: &Path,
    name: &str,
    expected: &T,
) -> Result<(), LayerError> {
    let marker = revision.join(name);
    reject_unsafe_layer_receipt(&marker)?;
    let bytes = std::fs::read(&marker).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            LayerError::NotInstalled(revision.display().to_string())
        } else {
            LayerError::Io(error)
        }
    })?;
    let actual: T = serde_json::from_slice(&bytes).map_err(|_| LayerError::Conflict)?;
    if actual != *expected {
        return Err(LayerError::Conflict);
    }
    Ok(())
}

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
        let target = existing_identity(&path)?;
        for other in &selection.backends {
            if existing_identity(&installer.installed_path(other)?)? == target {
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
        let target = existing_identity(&path)?;
        for other in &selection.overlays {
            if existing_identity(&installer.installed_path(other)?)? == target {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

fn assess(
    path: &Path,
    configured: &[ServiceSelection],
    running: &[ActiveWorkerLayers],
    selected: impl Fn(&ServiceSelection) -> Result<bool, LayerError>,
) -> Result<RemovalAssessment, LayerError> {
    if !path.is_absolute() {
        return Err(LayerError::Invalid("layer path must be absolute".into()));
    }
    let resolved_path = existing_identity(path)?;
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
        if worker
            .paths
            .iter()
            .map(|candidate| existing_identity(candidate))
            .collect::<Result<Vec<_>, _>>()?
            .contains(&resolved_path)
        {
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

fn existing_identity(path: &Path) -> Result<PathBuf, LayerError> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(path.to_owned()),
        Err(error) => Err(LayerError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::backend::BackendKind;
    use std::time::Duration;

    fn managed_installers() -> (PathBuf, BackendInstaller, OverlayInstaller) {
        let data = std::env::temp_dir().join(format!("nm-owned-removal-{}", Uuid::new_v4()));
        let (mut backends, mut overlays) = installers();
        backends.backends_directory = data.join("AIData/backends");
        overlays.overlays_directory = data.join("AIData/overlays");
        std::fs::create_dir_all(backends.backends_directory.join("remote-api/shared1")).unwrap();
        std::fs::create_dir_all(overlays.overlays_directory.join("tts_one/overlay1")).unwrap();
        std::fs::write(
            backends
                .backends_directory
                .join("remote-api/shared1/.neuromita-backend.json"),
            serde_json::to_vec(&backend()).unwrap(),
        )
        .unwrap();
        std::fs::write(
            overlays
                .overlays_directory
                .join("tts_one/overlay1/.neuromita-overlay.json"),
            serde_json::to_vec(&overlay("tts_one")).unwrap(),
        )
        .unwrap();
        (data, backends, overlays)
    }

    #[test]
    fn owned_backend_inspection_is_exclusive_and_preserves_selected_blockers() {
        let (root, backends, _) = managed_installers();
        let owner = SettingsInstanceLease::acquire(&root).unwrap();
        let chosen = backend();
        let path = backends.installed_path(&chosen).unwrap();
        let selected = [ServiceSelection {
            service: AiService::Asr,
            backends: vec![chosen.clone()],
            overlays: vec![],
        }];
        let inspection =
            inspect_backend_with_root(&owner, &root, &backends, &chosen, &selected, &[]).unwrap();
        assert_eq!(
            inspection.assessment.blockers,
            vec![RemovalBlocker::SelectedBy(AiService::Asr)],
        );
        assert_eq!(
            RevisionLease::shared(&path).err().unwrap().kind(),
            io::ErrorKind::WouldBlock,
        );
        drop(inspection);
        assert!(RevisionLease::shared(&path).is_ok());
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owned_overlay_inspection_rejects_foreign_root_and_receipt_mismatch() {
        let (root, _, overlays) = managed_installers();
        let foreign = std::env::temp_dir().join(format!("nm-foreign-removal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&foreign).unwrap();
        let owner = SettingsInstanceLease::acquire(&foreign).unwrap();
        let chosen = overlay("tts_one");
        assert!(matches!(
            inspect_overlay_with_root(&owner, &root, &overlays, &chosen, &[], &[]),
            Err(LayerError::Invalid(_))
        ));
        drop(owner);
        let owner = SettingsInstanceLease::acquire(&root).unwrap();
        let revision = overlays.installed_path(&chosen).unwrap();
        let marker = revision.join(".neuromita-overlay.json");
        let original = std::fs::read(&marker).unwrap();
        std::fs::write(&marker, b"{}").unwrap();
        assert!(matches!(
            inspect_overlay_with_root(&owner, &root, &overlays, &chosen, &[], &[]),
            Err(LayerError::Conflict)
        ));
        std::fs::write(&marker, &original).unwrap();
        let assessment =
            inspect_overlay_with_root(&owner, &root, &overlays, &chosen, &[], &[]).unwrap();
        assert!(!assessment.assessment.has_observed_references());
        drop(assessment);
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(foreign).unwrap();
    }

    #[test]
    fn active_foreign_python_import_blocks_exclusive_inspection() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let (root, backends, _) = managed_installers();
        let owner = SettingsInstanceLease::acquire(&root).unwrap();
        let chosen = backend();
        let revision = backends.installed_path(&chosen).unwrap();
        let ready = root.join("child-ready");
        let release = root.join("child-release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ai_runtime::revision_lease::tests::child_process_can_hold_shared_revision_lock",
            ])
            .env("NM_REVISION_LEASE_CHILD_PATH", &revision)
            .env("NM_REVISION_LEASE_CHILD_READY", &ready)
            .env("NM_REVISION_LEASE_CHILD_RELEASE", &release)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "child never acquired its revision lease"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before leasing revision"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(
            inspect_backend_with_root(&owner, &root, &backends, &chosen, &[], &[]),
            Err(LayerError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock
        ));
        std::fs::write(release, []).unwrap();
        assert!(child.wait().unwrap().success());
        let inspection =
            inspect_backend_with_root(&owner, &root, &backends, &chosen, &[], &[]).unwrap();
        assert!(!inspection.assessment.has_observed_references());
        drop(inspection);
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_managed_parent_is_not_a_deletion_inspection_root() {
        use std::os::unix::fs::symlink;
        let (root, backends, _) = managed_installers();
        let owner = SettingsInstanceLease::acquire(&root).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::rename(&backends.backends_directory, &elsewhere).unwrap();
        symlink(&elsewhere, &backends.backends_directory).unwrap();
        assert!(matches!(
            inspect_backend_with_root(&owner, &root, &backends, &backend(), &[], &[]),
            Err(LayerError::Invalid(_))
        ));
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }

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

    #[cfg(unix)]
    #[test]
    fn canonical_import_paths_cannot_bypass_selected_layer_assessment() {
        use std::os::unix::fs::symlink;

        let (mut backend_installer, _) = installers();
        let root = backend_installer
            .backends_directory
            .parent()
            .unwrap()
            .to_path_buf();
        let actual = root.join("actual/backends");
        std::fs::create_dir_all(actual.join("remote-api/shared1")).unwrap();
        symlink(&actual, &backend_installer.backends_directory).unwrap();
        let shared = backend();
        let path = backend_installer.installed_path(&shared).unwrap();
        let canonical = path.canonicalize().unwrap();
        let generation = Uuid::new_v4();
        let running = [ActiveWorkerLayers {
            generation,
            paths: vec![canonical],
        }];
        let assessment = assess_backend(&backend_installer, &shared, &[], &running).unwrap();
        assert_eq!(
            assessment.blockers,
            vec![RemovalBlocker::ActiveGeneration(generation)]
        );
        backend_installer.backends_directory = actual;
        let configured = [ServiceSelection {
            service: AiService::Tts,
            backends: vec![shared.clone()],
            overlays: vec![],
        }];
        let assessment =
            assess_backend(&backend_installer, &shared, &configured, &running).unwrap();
        assert_eq!(
            assessment.blockers,
            vec![
                RemovalBlocker::SelectedBy(AiService::Tts),
                RemovalBlocker::ActiveGeneration(generation)
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
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
