//! Admission and atomic publication of externally approved native executable packages.
//! This module copies packages; it never interprets or runs model code.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use ring::digest::{Context, SHA256};
use thiserror::Error;
use uuid::Uuid;

use super::{
    native_model::NativeBackend,
    native_registry::{
        NativeFileRef, NativeModelRegistry, NativeRegistryError, check_path, parse_sha256,
        read_manifest, verify_file,
    },
};

const MANIFEST_NAME: &str = "native-model.json";

#[derive(Debug, Error)]
pub enum NativePackageInstallError {
    #[error("the managed native package or runtime storage is busy")]
    Busy,
    #[error("a different immutable revision of this native model is already published")]
    RevisionConflict,
    #[error("a shared runtime path is already occupied by different or unsafe content")]
    RuntimeConflict,
    #[error("invalid native install layout: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Registry(#[from] NativeRegistryError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePackageInstallResult {
    pub model_id: String,
    pub manifest_sha256: [u8; 32],
    pub reused_package: bool,
    pub new_runtime_files: usize,
}

/// Both managed roots must belong to the application and be on filesystems
/// supporting atomic same-filesystem rename and hard links, respectively.
pub struct NativePackageInstaller {
    packages_root: PathBuf,
    runtime_root: PathBuf,
}

impl NativePackageInstaller {
    pub fn new(packages_root: PathBuf, runtime_root: PathBuf) -> Self {
        Self {
            packages_root,
            runtime_root,
        }
    }

    /// `approved_manifest_sha256` must come from an external trusted source.
    /// `source_*` are local, already downloaded locations, not remote URLs.
    pub fn install_local(
        &self,
        source_packages_root: &Path,
        source_runtime_root: &Path,
        model_id: &str,
        approved_manifest_sha256: [u8; 32],
        backend: NativeBackend,
    ) -> Result<NativePackageInstallResult, NativePackageInstallError> {
        for root in [
            &self.packages_root,
            &self.runtime_root,
            source_packages_root,
            source_runtime_root,
        ] {
            check_path(root, true)?;
        }
        let managed_packages = self.packages_root.canonicalize()?;
        let managed_runtime = self.runtime_root.canonicalize()?;
        let source_packages = source_packages_root.canonicalize()?;
        let source_runtime = source_runtime_root.canonicalize()?;
        for (left, right) in [
            (&managed_packages, &managed_runtime),
            (&managed_packages, &source_packages),
            (&managed_packages, &source_runtime),
            (&managed_runtime, &source_packages),
            (&managed_runtime, &source_runtime),
        ] {
            if left.starts_with(right) || right.starts_with(left) {
                return Err(NativePackageInstallError::Invalid(
                    "native source and managed roots must be disjoint",
                ));
            }
        }
        let package_stages = stage_root(&self.packages_root)?;
        let runtime_stages = stage_root(&self.runtime_root)?;
        if package_stages == runtime_stages {
            return Err(NativePackageInstallError::Invalid("staging roots overlap"));
        }
        let _package_owner = ManagedLock::acquire(&package_stages)?;
        let _runtime_owner = ManagedLock::acquire(&runtime_stages)?;
        reclaim(&package_stages)?;
        reclaim(&runtime_stages)?;

        let registration = NativeModelRegistry::discover(source_packages_root)?
            .into_iter()
            .find(|entry| entry.manifest().model_id == model_id)
            .ok_or(NativePackageInstallError::Invalid(
                "selected source model missing",
            ))?;
        registration.prepare_launch(
            approved_manifest_sha256,
            backend,
            source_runtime_root,
            Duration::from_secs(1),
        )?;
        let manifest = registration.manifest();
        let package_source = registration.package_directory();
        let manifest_bytes = read_manifest(&package_source.join(MANIFEST_NAME))?;
        if ring::digest::digest(&SHA256, &manifest_bytes).as_ref() != approved_manifest_sha256 {
            return Err(NativePackageInstallError::Registry(
                NativeRegistryError::ChangedManifest,
            ));
        }

        let destination = self.packages_root.join(model_id);
        let old = match fs::symlink_metadata(&destination) {
            Ok(_) => {
                let found = NativeModelRegistry::discover(&self.packages_root)?
                    .into_iter()
                    .find(|entry| entry.manifest().model_id == model_id)
                    .ok_or(NativePackageInstallError::RevisionConflict)?;
                if found.manifest_sha256() != approved_manifest_sha256 {
                    return Err(NativePackageInstallError::RevisionConflict);
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };

        let selected_runtime: Vec<&NativeFileRef> = manifest
            .runtime_files
            .iter()
            .filter(|runtime| runtime.backends.contains(&backend))
            .map(|runtime| &runtime.file)
            .collect();
        for file in &selected_runtime {
            if present(&self.runtime_root.join(&file.path))? {
                verify_file(&self.runtime_root, file)
                    .map_err(|_| NativePackageInstallError::RuntimeConflict)?;
            }
        }
        if old {
            let found = NativeModelRegistry::discover(&self.packages_root)?
                .into_iter()
                .find(|entry| entry.manifest().model_id == model_id)
                .ok_or(NativePackageInstallError::RevisionConflict)?;
            for file in std::iter::once(&manifest.executable).chain(manifest.assets.iter()) {
                verify_file(found.package_directory(), file)?;
            }
        }

        let mut package_stage = if old {
            None
        } else {
            let stage = OwnedStage::create(&package_stages)?;
            let manifest_path = stage.path.join(MANIFEST_NAME);
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&manifest_path)?;
            output.write_all(&manifest_bytes)?;
            output.sync_all()?;
            for file in std::iter::once(&manifest.executable).chain(manifest.assets.iter()) {
                verified_copy(
                    package_source,
                    &stage.path,
                    file,
                    file.path == manifest.executable.path,
                )?;
            }
            sync_dir(&stage.path)?;
            Some(stage)
        };
        let runtime_stage = OwnedStage::create(&runtime_stages)?;
        for file in &selected_runtime {
            if !present(&self.runtime_root.join(&file.path))? {
                verified_copy(source_runtime_root, &runtime_stage.path, file, false)?;
            }
        }
        sync_dir(&runtime_stage.path)?;

        let mut new_runtime_files = 0;
        for file in &selected_runtime {
            let dest = self.runtime_root.join(&file.path);
            if present(&dest)? {
                verify_file(&self.runtime_root, file)
                    .map_err(|_| NativePackageInstallError::RuntimeConflict)?;
                continue;
            }
            ensure_subdirectories(&self.runtime_root, &file.path)?;
            let staged = runtime_stage.path.join(&file.path);
            match fs::hard_link(&staged, &dest) {
                Ok(()) => {
                    sync_dir(
                        dest.parent()
                            .ok_or(NativePackageInstallError::Invalid("missing runtime parent"))?,
                    )?;
                    new_runtime_files += 1;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    verify_file(&self.runtime_root, file)
                        .map_err(|_| NativePackageInstallError::RuntimeConflict)?;
                }
                Err(error) => return Err(error.into()),
            }
        }

        if let Some(stage) = package_stage.as_mut() {
            if present(&destination)? {
                return Err(NativePackageInstallError::RevisionConflict);
            }
            fs::rename(&stage.path, &destination)?;
            stage.published = true;
            sync_dir(&self.packages_root)?;
        }
        let installed = NativeModelRegistry::discover(&self.packages_root)?
            .into_iter()
            .find(|entry| entry.manifest().model_id == model_id)
            .ok_or(NativePackageInstallError::Invalid(
                "published model disappeared",
            ))?;
        installed.prepare_launch(
            approved_manifest_sha256,
            backend,
            &self.runtime_root,
            Duration::from_secs(1),
        )?;
        Ok(NativePackageInstallResult {
            model_id: model_id.to_owned(),
            manifest_sha256: approved_manifest_sha256,
            reused_package: old,
            new_runtime_files,
        })
    }
}

struct ManagedLock {
    _owner: File,
}

impl ManagedLock {
    fn acquire(root: &Path) -> Result<Self, NativePackageInstallError> {
        let marker = root.join(".install-owner.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&marker)?;
        check_path(&marker, false)?;
        if file.metadata()?.len() != 0 {
            return Err(NativePackageInstallError::Invalid("invalid lock marker"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata()?;
            let now = fs::symlink_metadata(&marker)?;
            if opened.dev() != now.dev() || opened.ino() != now.ino() {
                return Err(NativePackageInstallError::Invalid("replaced lock marker"));
            }
        }
        match file.try_lock() {
            Ok(()) => Ok(Self { _owner: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(NativePackageInstallError::Busy),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

struct OwnedStage {
    path: PathBuf,
    published: bool,
}

impl OwnedStage {
    fn create(parent: &Path) -> Result<Self, NativePackageInstallError> {
        let path = parent.join(Uuid::new_v4().to_string());
        fs::create_dir(&path)?;
        Ok(Self {
            path,
            published: false,
        })
    }
}

impl Drop for OwnedStage {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn stage_root(managed: &Path) -> Result<PathBuf, NativePackageInstallError> {
    let parent = managed.parent().ok_or(NativePackageInstallError::Invalid(
        "managed storage has no parent",
    ))?;
    check_path(parent, true)?;
    let name = managed.file_name().and_then(|value| value.to_str()).ok_or(
        NativePackageInstallError::Invalid("invalid managed directory name"),
    )?;
    let root = parent.join(format!(".{name}.native-staging"));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&root) {
        Ok(()) => sync_dir(parent)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    check_path(&root, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&root)?.permissions().mode() & 0o022 != 0 {
            return Err(NativePackageInstallError::Invalid(
                "staging root must not be group/world writable",
            ));
        }
    }
    Ok(root)
}

fn reclaim(root: &Path) -> Result<(), NativePackageInstallError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(value) = name.to_str() else {
            continue;
        };
        if Uuid::parse_str(value).is_err() {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            return Err(NativePackageInstallError::Invalid(
                "unsafe stale staging entry",
            ));
        }
        fs::remove_dir_all(entry.path())?;
    }
    sync_dir(root)?;
    Ok(())
}

fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn ensure_subdirectories(root: &Path, relative: &str) -> Result<(), NativePackageInstallError> {
    check_path(root, true)?;
    let mut parent = root.to_path_buf();
    let Some((segments, _)) = relative.rsplit_once('/') else {
        return Ok(());
    };
    for segment in segments.split('/') {
        parent.push(segment);
        match fs::create_dir(&parent) {
            Ok(()) => sync_dir(parent.parent().ok_or(NativePackageInstallError::Invalid(
                "missing directory parent",
            ))?)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        check_path(&parent, true)?;
    }
    Ok(())
}

fn verified_copy(
    source_root: &Path,
    destination_root: &Path,
    reference: &NativeFileRef,
    executable: bool,
) -> Result<(), NativePackageInstallError> {
    let src = source_root.join(&reference.path);
    check_path(&src, false)?;
    ensure_subdirectories(destination_root, &reference.path)?;
    let dest = destination_root.join(&reference.path);
    let mut input = File::open(src)?;
    if input.metadata()?.len() != reference.size_bytes {
        return Err(NativePackageInstallError::Registry(
            NativeRegistryError::Integrity,
        ));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)?;
    let mut digest = Context::new(&SHA256);
    let mut count = 0_u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let bytes = input.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        count = count.saturating_add(bytes as u64);
        if count > reference.size_bytes {
            return Err(NativePackageInstallError::Registry(
                NativeRegistryError::Integrity,
            ));
        }
        digest.update(&buffer[..bytes]);
        output.write_all(&buffer[..bytes])?;
    }
    if count != reference.size_bytes || digest.finish().as_ref() != parse_sha256(&reference.sha256)?
    {
        return Err(NativePackageInstallError::Registry(
            NativeRegistryError::Integrity,
        ));
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mode = input.metadata()?.mode() & 0o755;
        if mode & 0o100 == 0 {
            return Err(NativePackageInstallError::Invalid(
                "native executable lacks owner execute permission",
            ));
        }
        output.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    output.sync_all()?;
    sync_dir(
        dest.parent()
            .ok_or(NativePackageInstallError::Invalid("missing file parent"))?,
    )?;
    verify_file(destination_root, reference)?;
    Ok(())
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}
