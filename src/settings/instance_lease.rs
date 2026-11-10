//! Persistent OS exclusion for one settings-owned portable data root.
//!
//! A running application holds this lock until its workers and final settings
//! flush have stopped. Maintenance uses the same exclusive ownership. It is a
//! lifecycle fence, not authorization to delete an installed AI revision.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

pub(crate) struct SettingsInstanceLease {
    _file: File,
    root: PathBuf,
}

impl SettingsInstanceLease {
    pub(crate) fn acquire(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings root must be an absolute injected directory",
            ));
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings root is not a directory",
            ));
        }
        let marker = root.join(".neuromita-settings-lifecycle.lock");
        if let Ok(metadata) = std::fs::symlink_metadata(&marker)
            && (!metadata.is_file() || metadata.len() != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe settings lifecycle lock marker",
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&marker)?;
        let metadata = std::fs::symlink_metadata(&marker)?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "settings lifecycle lock marker changed",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata()?;
            if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "settings lifecycle lock marker replaced",
                ));
            }
        }
        file.try_lock()?;
        Ok(Self { _file: file, root })
    }

    pub(crate) fn matches_root(&self, root: &Path) -> io::Result<bool> {
        Ok(self.root == root.canonicalize()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    fn root() -> PathBuf {
        let path = std::env::temp_dir().join(format!("nm-settings-instance-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn stable_marker_rejects_concurrent_owner_and_reacquires_after_release() {
        let root = root();
        let first = SettingsInstanceLease::acquire(&root).unwrap();
        assert_eq!(
            SettingsInstanceLease::acquire(&root).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(first.matches_root(&root).unwrap());
        drop(first);
        assert!(SettingsInstanceLease::acquire(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn subprocess_owner_fixture() {
        let Ok(root) = std::env::var("NM_SETTINGS_INSTANCE_CHILD_ROOT") else {
            return;
        };
        let ready = PathBuf::from(std::env::var("NM_SETTINGS_INSTANCE_CHILD_READY").unwrap());
        let _lease = SettingsInstanceLease::acquire(Path::new(&root)).unwrap();
        std::fs::write(ready, "ready").unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn separate_process_is_excluded_and_sigkill_releases_ownership() {
        let root = root();
        let ready = root.join("ready");
        let mut owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "settings::instance_lease::tests::subprocess_owner_fixture",
            ])
            .env("NM_SETTINGS_INSTANCE_CHILD_ROOT", &root)
            .env("NM_SETTINGS_INSTANCE_CHILD_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "owner never acquired lifecycle lock"
            );
            assert!(owner.try_wait().unwrap().is_none(), "owner exited early");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            SettingsInstanceLease::acquire(&root).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        owner.kill().unwrap();
        owner.wait().unwrap();
        assert!(SettingsInstanceLease::acquire(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlink_marker_cannot_be_used_as_lock() {
        let root = root();
        let marker = root.join(".neuromita-settings-lifecycle.lock");
        let outside = root.join("outside");
        std::fs::write(&outside, "external").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &marker).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, &marker).unwrap();
        assert_eq!(
            SettingsInstanceLease::acquire(&root).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
