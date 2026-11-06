//! Executable-relative ownership of NeuroMita's persistent application data.
//!
//! Resolve once at the application boundary and inject paths into each owner.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDataRoot {
    path: PathBuf,
}

impl PortableDataRoot {
    /// An explicit absolute directory takes precedence over the environment;
    /// otherwise data lives in `UserData` beside the running executable.
    pub fn resolve(explicit_directory: Option<&Path>) -> io::Result<Self> {
        let executable = std::env::current_exe()?;
        Self::from_executable(
            &executable,
            explicit_directory,
            std::env::var_os("NEUROMITA_DATA_DIR").as_deref(),
        )
    }

    /// Pure, injectable resolution for callers and relocation tests.
    pub fn from_executable(
        executable: &Path,
        explicit_directory: Option<&Path>,
        environment_directory: Option<&OsStr>,
    ) -> io::Result<Self> {
        let selected = if let Some(directory) = explicit_directory {
            directory.to_path_buf()
        } else if let Some(directory) = environment_directory {
            PathBuf::from(directory)
        } else {
            if !executable.is_absolute() || executable.file_name().is_none() {
                return Err(invalid("executable path must be absolute and name a file"));
            }
            executable
                .parent()
                .ok_or_else(|| invalid("executable has no installation directory"))?
                .join("UserData")
        };
        if !selected.is_absolute() || selected.as_os_str().is_empty() {
            return Err(invalid(
                "data directory must be an absolute path (--data-dir or NEUROMITA_DATA_DIR)",
            ));
        }
        Ok(Self { path: selected })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Explicitly fail on read-only installations instead of falling back to a profile directory.
    pub fn ensure_writable(&self) -> io::Result<()> {
        self.probe_directory().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "portable data directory {} is not writable: {error}",
                    self.path.display()
                ),
            )
        })
    }

    fn probe_directory(&self) -> io::Result<()> {
        fs::create_dir_all(&self.path)?;
        let probe = self
            .path
            .join(format!(".neuromita-write-test-{}", Uuid::new_v4()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        drop(file);
        fs::remove_file(probe)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
