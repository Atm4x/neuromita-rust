//! Immutable native package revisions and atomic explicit version selection.
//! This module owns selection metadata, not installed contents or worker readiness.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{
    native_approval::VerifiedNativeRelease,
    native_controller::{NativeModelController, NativeModelSelection},
    native_model::NativeBackend,
    native_package_install::{
        ManagedLock, NativePackageInstallError, NativePackageInstallResult, NativePackageInstaller,
        sync_dir,
    },
    native_registry::{
        NativeModelRegistration, NativeModelRegistry, NativeRegistryError, check_path,
        parse_sha256, valid_identifier,
    },
};

const OBJECTS_DIR: &str = "objects";
const SELECTIONS_DIR: &str = "selections";
const SELECTION_SCHEMA: u32 = 1;
const MAX_OBJECTS: usize = 256;
const MAX_SELECTION_BYTES: u64 = 512;

#[derive(Debug, Error)]
pub enum NativeRevisionError {
    #[error("invalid native revision layout: {0}")]
    Invalid(&'static str),
    #[error("requested native revision is not published")]
    NotFound,
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Install(#[from] NativePackageInstallError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePublishedRevision {
    pub model_id: String,
    pub manifest_sha256: [u8; 32],
    pub package_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRevisionChoice {
    pub model_id: String,
    pub manifest_sha256: [u8; 32],
    pub backend: NativeBackend,
}

/// Metadata-only read model. None of these witnesses establish package-file integrity
/// or readiness of a resident model process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRevisionOverview {
    pub revisions: Vec<NativeRevisionMetadata>,
    pub selections: Vec<NativeSelectionMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRevisionMetadata {
    pub model_id: String,
    pub display_name: String,
    pub package_version: String,
    pub manifest_sha256: [u8; 32],
    pub backends: Vec<NativeBackend>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSelectionManifestState {
    Present,
    Missing,
    BackendUndeclared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSelectionMetadata {
    pub choice: NativeRevisionChoice,
    pub manifest_state: NativeSelectionManifestState,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionRecord {
    schema_version: u32,
    model_id: String,
    manifest_sha256: String,
    backend: NativeBackend,
}

/// Root is application-owned and must exist. This store never deletes a published
/// revision, including after changing selection, so resident workers keep their files.
/// `objects/<manifest-sha256>/<model-id>/native-model.json` is the only package catalog.
/// `selections/<model-id>.json` is a pointer to an explicitly chosen revision, not a receipt.
#[derive(Debug, Clone)]
pub struct NativeRevisionStore {
    root: PathBuf,
    runtime_root: PathBuf,
}

impl NativeRevisionStore {
    pub fn new(root: PathBuf, runtime_root: PathBuf) -> Self {
        Self { root, runtime_root }
    }

    /// Signed admission uses a key independently pinned by the caller.
    /// The underlying installer still rehashes every referenced artifact.
    pub fn install_verified_local(
        &self,
        source_packages_root: &Path,
        source_runtime_root: &Path,
        approved: &VerifiedNativeRelease,
        backend: NativeBackend,
    ) -> Result<NativePackageInstallResult, NativeRevisionError> {
        self.install_local(
            source_packages_root,
            source_runtime_root,
            approved.model_id(),
            approved.manifest_sha256(),
            backend,
        )
    }

    pub fn install_local(
        &self,
        source_packages_root: &Path,
        source_runtime_root: &Path,
        model_id: &str,
        approved_manifest_sha256: [u8; 32],
        backend: NativeBackend,
    ) -> Result<NativePackageInstallResult, NativeRevisionError> {
        self.verify_roots()?;
        if !valid_identifier(model_id) {
            return Err(NativeRevisionError::Invalid("invalid model id"));
        }
        for source in [source_packages_root, source_runtime_root] {
            check_path(source, true)?;
            let source = source.canonicalize()?;
            let managed = self.root.canonicalize()?;
            let runtime = self.runtime_root.canonicalize()?;
            if source.starts_with(&managed)
                || managed.starts_with(&source)
                || source.starts_with(&runtime)
                || runtime.starts_with(&source)
            {
                return Err(NativeRevisionError::Invalid(
                    "native source and managed storage must be disjoint",
                ));
            }
        }
        let source = NativeModelRegistry::discover(source_packages_root)?
            .into_iter()
            .find(|item| item.manifest().model_id == model_id)
            .ok_or(NativeRevisionError::NotFound)?;
        source.prepare_launch(
            approved_manifest_sha256,
            backend,
            source_runtime_root,
            Duration::from_secs(1),
        )?;
        let objects = self.root.join(OBJECTS_DIR);
        ensure_directory(&objects)?;
        let revision_root = objects.join(digest_hex(&approved_manifest_sha256));
        ensure_directory(&revision_root)?;
        Ok(
            NativePackageInstaller::new(revision_root, self.runtime_root.clone()).install_local(
                source_packages_root,
                source_runtime_root,
                model_id,
                approved_manifest_sha256,
                backend,
            )?,
        )
    }

    /// Lists published manifests, not inferred worker readiness or install receipts.
    pub fn list(
        &self,
        model_id: &str,
    ) -> Result<Vec<NativePublishedRevision>, NativeRevisionError> {
        self.verify_roots()?;
        if !valid_identifier(model_id) {
            return Err(NativeRevisionError::Invalid("invalid model id"));
        }
        let mut revisions = Vec::new();
        for (digest, registration) in self.all_registrations()? {
            if registration.manifest().model_id == model_id {
                revisions.push(NativePublishedRevision {
                    model_id: model_id.to_owned(),
                    manifest_sha256: digest,
                    package_version: registration.manifest().package_version.clone(),
                });
            }
        }
        revisions.sort_by(|a, b| a.manifest_sha256.cmp(&b.manifest_sha256));
        Ok(revisions)
    }

    /// Switches only the durable selection file; never kills or redirects a live worker.
    /// Approval must be an independently authenticated digest from the application.
    pub fn select(&self, choice: &NativeRevisionChoice) -> Result<(), NativeRevisionError> {
        self.verify_roots()?;
        self.registration_for(choice)?.prepare_launch(
            choice.manifest_sha256,
            choice.backend,
            &self.runtime_root,
            Duration::from_secs(1),
        )?;
        let selections = self.root.join(SELECTIONS_DIR);
        ensure_directory(&selections)?;
        let _guard = ManagedLock::acquire(&selections)?;
        reclaim_selection_temps(&selections)?;
        let file_path = selections.join(format!("{}.json", choice.model_id));
        check_existing_regular(&file_path)?;
        let record = SelectionRecord {
            schema_version: SELECTION_SCHEMA,
            model_id: choice.model_id.clone(),
            manifest_sha256: digest_hex(&choice.manifest_sha256),
            backend: choice.backend,
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() as u64 > MAX_SELECTION_BYTES {
            return Err(NativeRevisionError::Invalid("selection record too large"));
        }
        let temporary = selections.join(format!(".select-{}.tmp", Uuid::new_v4()));
        let mut guard = TemporarySelection::create(temporary)?;
        guard.file.write_all(&bytes)?;
        guard.file.sync_all()?;
        fs::rename(&guard.path, &file_path)?;
        guard.committed = true;
        sync_dir(&selections)?;
        Ok(())
    }

    /// Reads an atomic selection snapshot, then verifies its full package and runtime contents.
    /// An absent selection is not an implicit request for the newest published revision.
    pub fn selected(
        &self,
        model_id: &str,
    ) -> Result<Option<NativeRevisionChoice>, NativeRevisionError> {
        let Some(choice) = self.selection_hint(model_id)? else {
            return Ok(None);
        };
        self.registration_for(&choice)?.prepare_launch(
            choice.manifest_sha256,
            choice.backend,
            &self.runtime_root,
            Duration::from_secs(1),
        )?;
        Ok(Some(choice))
    }

    /// Cheap, strictly parsed on-disk pointer. This does not check model weights,
    /// executable bytes or runtime libraries and MUST NOT authorize execution.
    pub fn selection_hint(
        &self,
        model_id: &str,
    ) -> Result<Option<NativeRevisionChoice>, NativeRevisionError> {
        self.verify_roots()?;
        if !valid_identifier(model_id) {
            return Err(NativeRevisionError::Invalid("invalid model id"));
        }
        let directory = self.root.join(SELECTIONS_DIR);
        if !exists(&directory)? {
            return Ok(None);
        }
        check_path(&directory, true)?;
        let file_path = directory.join(format!("{model_id}.json"));
        if !exists(&file_path)? {
            return Ok(None);
        }
        check_path(&file_path, false)?;
        let file = File::open(file_path)?;
        let length = file.metadata()?.len();
        if length == 0 || length > MAX_SELECTION_BYTES {
            return Err(NativeRevisionError::Invalid("selection record size"));
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take(MAX_SELECTION_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != length {
            return Err(NativeRevisionError::Invalid("selection record changed"));
        }
        let record: SelectionRecord = serde_json::from_slice(&bytes)?;
        if record.schema_version != SELECTION_SCHEMA || record.model_id != model_id {
            return Err(NativeRevisionError::Invalid("selection identity mismatch"));
        }
        let digest = parse_sha256(&record.manifest_sha256)?;
        if record.manifest_sha256 != digest_hex(&digest) {
            return Err(NativeRevisionError::Invalid("noncanonical selected digest"));
        }
        let choice = NativeRevisionChoice {
            model_id: model_id.to_owned(),
            manifest_sha256: digest,
            backend: record.backend,
        };
        Ok(Some(choice))
    }

    /// Bounded UI inventory. Reads *only* small manifest and selection JSON files;
    /// no EXE/weights/runtime SHA scan occurs. A selection is a user preference,
    /// not a proof that its package is usable, loaded or ready for inference.
    pub fn overview(&self) -> Result<NativeRevisionOverview, NativeRevisionError> {
        self.verify_roots()?;
        let mut revisions = Vec::new();
        for (digest, registration) in self.all_registrations()? {
            let manifest = registration.manifest();
            revisions.push(NativeRevisionMetadata {
                model_id: manifest.model_id.clone(),
                display_name: manifest.display_name.clone(),
                package_version: manifest.package_version.clone(),
                manifest_sha256: digest,
                backends: manifest.backends.clone(),
            });
        }
        revisions.sort_by(|left, right| {
            left.model_id
                .cmp(&right.model_id)
                .then(left.manifest_sha256.cmp(&right.manifest_sha256))
        });

        let mut selections = Vec::new();
        let directory = self.root.join(SELECTIONS_DIR);
        if exists(&directory)? {
            check_path(&directory, true)?;
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or(NativeRevisionError::Invalid("non-utf8 selection entry"))?;
                if name == ".install-owner.lock" {
                    check_path(&entry.path(), false)?;
                    continue;
                }
                if let Some(uuid) = name
                    .strip_prefix(".select-")
                    .and_then(|value| value.strip_suffix(".tmp"))
                {
                    if Uuid::parse_str(uuid).is_err() {
                        return Err(NativeRevisionError::Invalid("invalid selection temp"));
                    }
                    check_path(&entry.path(), false)?;
                    continue;
                }
                let model_id = name
                    .strip_suffix(".json")
                    .filter(|id| valid_identifier(id))
                    .ok_or(NativeRevisionError::Invalid("unexpected selection entry"))?;
                if selections.len() == MAX_OBJECTS {
                    return Err(NativeRevisionError::Invalid("too many selections"));
                }
                let choice = self
                    .selection_hint(model_id)?
                    .ok_or(NativeRevisionError::Invalid("selection disappeared"))?;
                let manifest_state = match revisions.iter().find(|revision| {
                    revision.model_id == choice.model_id
                        && revision.manifest_sha256 == choice.manifest_sha256
                }) {
                    None => NativeSelectionManifestState::Missing,
                    Some(revision) if !revision.backends.contains(&choice.backend) => {
                        NativeSelectionManifestState::BackendUndeclared
                    }
                    Some(_) => NativeSelectionManifestState::Present,
                };
                selections.push(NativeSelectionMetadata {
                    choice,
                    manifest_state,
                });
            }
        }
        selections.sort_by(|a, b| a.choice.model_id.cmp(&b.choice.model_id));
        Ok(NativeRevisionOverview {
            revisions,
            selections,
        })
    }

    /// Caller owns the returned controller for its entire worker session. A later
    /// selection change cannot mutate its package directory or redirect its IPC.
    pub fn controller_for_selected(
        &self,
        model_id: &str,
        timeout: Duration,
    ) -> Result<Option<(NativeModelController, NativeModelSelection)>, NativeRevisionError> {
        if timeout.is_zero() {
            return Err(NativeRevisionError::Invalid("zero request timeout"));
        }
        let Some(choice) = self.selected(model_id)? else {
            return Ok(None);
        };
        let controller = NativeModelController::new(
            self.object_root(&choice.manifest_sha256),
            self.runtime_root.clone(),
        );
        let selection = NativeModelSelection {
            model_id: choice.model_id,
            approved_manifest_sha256: choice.manifest_sha256,
            backend: choice.backend,
            request_timeout: timeout,
        };
        Ok(Some((controller, selection)))
    }

    fn registration_for(
        &self,
        choice: &NativeRevisionChoice,
    ) -> Result<NativeModelRegistration, NativeRevisionError> {
        if !valid_identifier(&choice.model_id) {
            return Err(NativeRevisionError::Invalid("invalid model id"));
        }
        let root = self.object_root(&choice.manifest_sha256);
        if !exists(&root)? {
            return Err(NativeRevisionError::NotFound);
        }
        let mut matches = NativeModelRegistry::discover(&root)?;
        if matches.len() > 1 {
            return Err(NativeRevisionError::Invalid(
                "multiple models under one digest",
            ));
        }
        let registration = matches.pop().ok_or(NativeRevisionError::NotFound)?;
        if registration.manifest().model_id != choice.model_id {
            return Err(NativeRevisionError::NotFound);
        }
        if registration.manifest_sha256() != choice.manifest_sha256 {
            return Err(NativeRevisionError::Invalid(
                "revision directory digest mismatch",
            ));
        }
        Ok(registration)
    }

    fn object_root(&self, digest: &[u8; 32]) -> PathBuf {
        self.root.join(OBJECTS_DIR).join(digest_hex(digest))
    }

    /// Transfer staging must be separate from both published model revisions and
    /// the shared runtime. Fail before making a network request or creating a stage.
    pub(super) fn verify_download_staging_root(
        &self,
        staging_root: &Path,
    ) -> Result<(), NativeRevisionError> {
        self.verify_roots()?;
        check_path(staging_root, true)?;
        let staging = staging_root.canonicalize()?;
        for managed in [&self.root, &self.runtime_root] {
            let managed = managed.canonicalize()?;
            if staging.starts_with(&managed) || managed.starts_with(&staging) {
                return Err(NativeRevisionError::Invalid(
                    "native download staging overlaps managed storage",
                ));
            }
        }
        Ok(())
    }

    fn verify_roots(&self) -> Result<(), NativeRevisionError> {
        check_path(&self.root, true)?;
        check_path(&self.runtime_root, true)?;
        let root = self.root.canonicalize()?;
        let runtime = self.runtime_root.canonicalize()?;
        if root.starts_with(&runtime) || runtime.starts_with(&root) {
            return Err(NativeRevisionError::Invalid(
                "native revision and runtime roots overlap",
            ));
        }
        Ok(())
    }

    fn all_registrations(
        &self,
    ) -> Result<Vec<([u8; 32], NativeModelRegistration)>, NativeRevisionError> {
        let objects = self.root.join(OBJECTS_DIR);
        if !exists(&objects)? {
            return Ok(Vec::new());
        }
        check_path(&objects, true)?;
        let mut result = Vec::new();
        let mut count = 0;
        for entry in fs::read_dir(&objects)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or(NativeRevisionError::Invalid("non-utf8 revision entry"))?;
            if let Some(value) = name
                .strip_prefix('.')
                .and_then(|value| value.strip_suffix(".native-staging"))
            {
                validate_digest_name(value)?;
                check_path(&entry.path(), true)?;
                continue;
            }
            let digest = validate_digest_name(name)?;
            if count == MAX_OBJECTS {
                return Err(NativeRevisionError::Invalid("too many native revisions"));
            }
            count += 1;
            check_path(&entry.path(), true)?;
            let registrations = NativeModelRegistry::discover(&entry.path())?;
            if registrations.len() > 1 {
                return Err(NativeRevisionError::Invalid(
                    "multiple models under one digest",
                ));
            }
            for registration in registrations {
                if registration.manifest_sha256() != digest {
                    return Err(NativeRevisionError::Invalid(
                        "revision directory digest mismatch",
                    ));
                }
                result.push((digest, registration));
            }
        }
        Ok(result)
    }
}

struct TemporarySelection {
    path: PathBuf,
    file: File,
    committed: bool,
}

impl TemporarySelection {
    fn create(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            path,
            file,
            committed: false,
        })
    }
}

impl Drop for TemporarySelection {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn digest_hex(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_digest_name(value: &str) -> Result<[u8; 32], NativeRevisionError> {
    let digest = parse_sha256(value)?;
    if value != digest_hex(&digest) {
        return Err(NativeRevisionError::Invalid("noncanonical revision digest"));
    }
    Ok(digest)
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn ensure_directory(path: &Path) -> Result<(), NativeRevisionError> {
    let parent = path.parent().ok_or(NativeRevisionError::Invalid(
        "missing managed directory parent",
    ))?;
    check_path(parent, true)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => sync_dir(parent)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    check_path(path, true)?;
    Ok(())
}

fn check_existing_regular(path: &Path) -> Result<(), NativeRevisionError> {
    if exists(path)? {
        check_path(path, false)?;
    }
    Ok(())
}

fn reclaim_selection_temps(root: &Path) -> Result<(), NativeRevisionError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(uuid) = name
            .strip_prefix(".select-")
            .and_then(|value| value.strip_suffix(".tmp"))
        else {
            continue;
        };
        if Uuid::parse_str(uuid).is_err() {
            continue;
        }
        check_path(&entry.path(), false)?;
        fs::remove_file(entry.path())?;
    }
    sync_dir(root)?;
    Ok(())
}
