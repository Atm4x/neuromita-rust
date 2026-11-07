//! Advisory OS ownership for one immutable installed package revision.
//!
//! The sibling lock file is deliberately persistent: unlinking it allows two
//! owners to lock different inodes for the same revision. Every participant
//! resolves the managed parent before accessing this marker.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

pub(crate) struct RevisionLease {
    _file: File,
}

impl RevisionLease {
    pub(crate) fn shared(revision: &Path) -> io::Result<Self> {
        let file = open_lease(revision)?;
        file.try_lock_shared()?;
        if !revision.is_dir() || revision.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "installed revision disappeared before worker launch",
            ));
        }
        Ok(Self { _file: file })
    }

    pub(crate) fn exclusive(revision: &Path) -> io::Result<Self> {
        let file = open_lease(revision)?;
        file.try_lock()?;
        Ok(Self { _file: file })
    }
}

pub(crate) fn shared_layers(revisions: &[PathBuf]) -> io::Result<Vec<RevisionLease>> {
    let mut ordered = revisions.to_vec();
    ordered.sort_unstable();
    ordered.dedup();
    ordered
        .iter()
        .map(|path| RevisionLease::shared(path))
        .collect()
}

fn open_lease(revision: &Path) -> io::Result<File> {
    if !revision.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "revision must have an absolute path",
        ));
    }
    let name = revision
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| super::layer::valid_segment(name))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid revision name"))?;
    let parent = revision
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing managed parent"))?
        .canonicalize()?;
    let lock = parent.join(format!(".neuromita-revision-{name}.lock"));
    if let Ok(metadata) = std::fs::symlink_metadata(&lock)
        && (!metadata.is_file() || metadata.len() != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsafe revision ownership marker",
        ));
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)?;
    let metadata = std::fs::symlink_metadata(&lock)?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "revision ownership marker changed",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "revision ownership marker replaced",
            ));
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn revision() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("nm-revision-lease-{}", Uuid::new_v4()));
        let revision = root.join("layers").join("r1");
        std::fs::create_dir_all(&revision).unwrap();
        (root, revision)
    }

    #[test]
    fn multiple_readers_block_exclusive_mutation_until_the_last_reader_exits() {
        let (root, path) = revision();
        let first = RevisionLease::shared(&path).unwrap();
        let second = RevisionLease::shared(&path).unwrap();
        assert_eq!(
            RevisionLease::exclusive(&path).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(first);
        assert_eq!(
            RevisionLease::exclusive(&path).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(second);
        let writer = RevisionLease::exclusive(&path).unwrap();
        assert_eq!(
            RevisionLease::shared(&path).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(writer);
        assert!(RevisionLease::shared(&path).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_lease_inode_is_preserved_after_the_owner_is_dropped() {
        let (root, path) = revision();
        let owner = RevisionLease::shared(&path).unwrap();
        let marker = path.parent().unwrap().join(".neuromita-revision-r1.lock");
        assert_eq!(std::fs::metadata(&marker).unwrap().len(), 0);
        assert!(marker.is_file());
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn child_process_can_hold_shared_revision_lock() {
        let Ok(revision) = std::env::var("NM_REVISION_LEASE_CHILD_PATH") else {
            return;
        };
        let ready = std::env::var("NM_REVISION_LEASE_CHILD_READY").unwrap();
        let release = std::env::var("NM_REVISION_LEASE_CHILD_RELEASE").unwrap();
        let _guard = RevisionLease::shared(Path::new(&revision)).unwrap();
        std::fs::write(ready, b"ready").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
        while !Path::new(&release).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child lease release timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    #[test]
    fn another_os_process_cannot_mutate_a_shared_held_revision() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let (root, path) = revision();
        let ready = root.join("ready");
        let release = root.join("release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ai_runtime::revision_lease::tests::child_process_can_hold_shared_revision_lock",
            ])
            .env("NM_REVISION_LEASE_CHILD_PATH", &path)
            .env("NM_REVISION_LEASE_CHILD_READY", &ready)
            .env("NM_REVISION_LEASE_CHILD_RELEASE", &release)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not acquire its revision lease"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            RevisionLease::exclusive(&path).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        std::fs::write(&release, []).unwrap();
        assert!(child.wait().unwrap().success());
        assert!(RevisionLease::exclusive(&path).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn installed_receipt_readers_refuse_an_exclusive_revision_owner() {
        use crate::ai_runtime::layer::{LayerError, LayerInstaller};
        use std::time::Duration;

        let (root, revision) = revision();
        let marker = ".receipt.json";
        let plan = serde_json::json!({ "revision": "r1" });
        std::fs::write(revision.join(marker), serde_json::to_vec(&plan).unwrap()).unwrap();
        let installer = LayerInstaller {
            uv_executable: &root,
            python_executable: &root,
            cache_directory: &root,
            timeout: Duration::from_secs(1),
        };
        let writer = RevisionLease::exclusive(&revision).unwrap();
        assert!(matches!(
            installer.installed(&revision, marker, &plan, &[], &[]).await,
            Err(LayerError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock
        ));
        drop(writer);
        assert!(
            installer
                .installed(&revision, marker, &plan, &[], &[])
                .await
                .unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_replace_the_revision_lease_marker() {
        use std::os::unix::fs::symlink;
        let (root, path) = revision();
        let outside = root.join("outside");
        std::fs::write(&outside, []).unwrap();
        let marker = path.parent().unwrap().join(".neuromita-revision-r1.lock");
        symlink(&outside, &marker).unwrap();
        assert_eq!(
            RevisionLease::shared(&path).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
