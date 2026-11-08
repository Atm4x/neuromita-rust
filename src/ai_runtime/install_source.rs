//! Bounded source identity captured before dynamic install-plan execution.
//! The identity is rechecked before each production installation stage.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use ring::digest::{Context, SHA256};
use thiserror::Error;

const MAX_FILES: usize = 1024;
const MAX_DIRECTORIES: usize = 1024;
const MAX_DEPTH: usize = 24;
const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum InstallSourceError {
    #[error("unsafe or missing production installation source: {0}")]
    Unsafe(&'static str),
    #[error("production installation source changed after preflight")]
    Changed,
    #[error("production installation source I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct InstallSourcePin {
    plugins_directory: PathBuf,
    plugin_ids: Vec<String>,
    python_executable: PathBuf,
    uv_executable: PathBuf,
    digest: String,
}

fn checked_directory(path: &Path) -> Result<(), InstallSourceError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(InstallSourceError::Unsafe(
            "plugin directory is not a real directory",
        ));
    }
    Ok(())
}

fn hash_file(
    path: &Path,
    ctx: &mut Context,
    remaining: &mut u64,
) -> Result<(), InstallSourceError> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > MAX_FILE_BYTES {
        return Err(InstallSourceError::Unsafe(
            "unsafe or oversized installation source file",
        ));
    }
    *remaining = remaining
        .checked_sub(before.len())
        .ok_or(InstallSourceError::Unsafe(
            "installation source tree exceeds bounded size",
        ))?;
    let mut file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != before.len() {
        return Err(InstallSourceError::Changed);
    }
    ctx.update(&before.len().to_le_bytes());
    let mut buf = [0u8; 64 * 1024];
    let mut read_total = 0u64;
    loop {
        let count = file.read(&mut buf)?;
        if count == 0 {
            break;
        }
        read_total += count as u64;
        if read_total > before.len() {
            return Err(InstallSourceError::Changed);
        }
        ctx.update(&buf[..count]);
    }
    let after = fs::symlink_metadata(path)?;
    if read_total != before.len()
        || !after.is_file()
        || after.file_type().is_symlink()
        || after.len() != before.len()
    {
        return Err(InstallSourceError::Changed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err(InstallSourceError::Changed);
        }
    }
    Ok(())
}

fn hash_directory(
    root: &Path,
    relative: &Path,
    ctx: &mut Context,
    files: &mut usize,
    directories: &mut usize,
    depth: usize,
    remaining: &mut u64,
) -> Result<(), InstallSourceError> {
    *directories += 1;
    if *directories > MAX_DIRECTORIES || depth > MAX_DEPTH {
        return Err(InstallSourceError::Unsafe(
            "plugin source directory limit exceeded",
        ));
    }
    checked_directory(&root.join(relative))?;
    let mut entries = fs::read_dir(root.join(relative))?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let path = relative.join(&name);
        let absolute = root.join(&path);
        let metadata = fs::symlink_metadata(&absolute)?;
        if metadata.file_type().is_symlink() {
            return Err(InstallSourceError::Unsafe("symlink in plugin source tree"));
        }
        if metadata.is_dir() {
            hash_directory(root, &path, ctx, files, directories, depth + 1, remaining)?;
        } else if metadata.is_file() {
            *files += 1;
            if *files > MAX_FILES {
                return Err(InstallSourceError::Unsafe("too many plugin source files"));
            }
            let name = path
                .to_str()
                .ok_or(InstallSourceError::Unsafe("non-UTF8 plugin path"))?;
            ctx.update(&(name.len() as u64).to_le_bytes());
            ctx.update(name.as_bytes());
            hash_file(&absolute, ctx, remaining)?;
        } else {
            return Err(InstallSourceError::Unsafe(
                "non-regular plugin source entry",
            ));
        }
    }
    Ok(())
}

impl InstallSourcePin {
    pub fn capture(
        plugins_directory: &Path,
        plugin_ids: impl IntoIterator<Item = String>,
        python_executable: &Path,
        uv_executable: &Path,
    ) -> Result<Self, InstallSourceError> {
        let ids = plugin_ids
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut pin = Self {
            plugins_directory: plugins_directory.to_path_buf(),
            plugin_ids: ids,
            python_executable: python_executable.to_path_buf(),
            uv_executable: uv_executable.to_path_buf(),
            digest: String::new(),
        };
        pin.digest = pin.calculate()?;
        Ok(pin)
    }

    fn calculate(&self) -> Result<String, InstallSourceError> {
        if !self.plugins_directory.is_absolute()
            || !self.python_executable.is_absolute()
            || !self.uv_executable.is_absolute()
            || self.plugin_ids.is_empty()
        {
            return Err(InstallSourceError::Unsafe("unmanaged source path"));
        }
        checked_directory(&self.plugins_directory)?;
        let mut ctx = Context::new(&SHA256);
        let mut remaining = MAX_TOTAL_BYTES;
        let mut files = 0usize;
        let mut directories = 0usize;
        let plugin_root = self
            .plugins_directory
            .to_str()
            .ok_or(InstallSourceError::Unsafe("non-UTF8 plugin root"))?;
        ctx.update(&(plugin_root.len() as u64).to_le_bytes());
        ctx.update(plugin_root.as_bytes());
        for (label, path) in [
            ("python", &self.python_executable),
            ("uv", &self.uv_executable),
        ] {
            ctx.update(label.as_bytes());
            hash_file(path, &mut ctx, &mut remaining)?;
        }
        for id in &self.plugin_ids {
            if !super::layer::valid_segment(id) {
                return Err(InstallSourceError::Unsafe("invalid plugin identifier"));
            }
            ctx.update(&(id.len() as u64).to_le_bytes());
            ctx.update(id.as_bytes());
            hash_directory(
                &self.plugins_directory,
                Path::new(id),
                &mut ctx,
                &mut files,
                &mut directories,
                0,
                &mut remaining,
            )?;
        }
        Ok(ctx
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn verify(&self) -> Result<(), InstallSourceError> {
        if self.calculate()? == self.digest {
            Ok(())
        } else {
            Err(InstallSourceError::Changed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn source_identity_detects_plugin_and_toolchain_changes() {
        let root = std::env::temp_dir().join(format!("nm-source-pin-{}", Uuid::new_v4()));
        let plugins = root.join("plugins");
        fs::create_dir_all(plugins.join("sample")).unwrap();
        fs::write(plugins.join("sample/installplan.py"), b"plan-v1").unwrap();
        fs::write(plugins.join("sample/plugin.json"), b"manifest-v1").unwrap();
        let python = root.join("python");
        let uv = root.join("uv");
        fs::write(&python, b"python-v1").unwrap();
        fs::write(&uv, b"uv-v1").unwrap();
        let pin = InstallSourcePin::capture(&plugins, vec!["sample".into()], &python, &uv).unwrap();
        pin.verify().unwrap();
        fs::write(plugins.join("sample/installplan.py"), b"plan-v2").unwrap();
        assert!(matches!(pin.verify(), Err(InstallSourceError::Changed)));
        let pin = InstallSourcePin::capture(&plugins, vec!["sample".into()], &python, &uv).unwrap();
        fs::write(&uv, b"uv-v2").unwrap();
        assert!(matches!(pin.verify(), Err(InstallSourceError::Changed)));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn source_identity_refuses_symlink_and_oversized_plugin_sources() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("nm-source-pin-{}", Uuid::new_v4()));
        let plugins = root.join("plugins");
        fs::create_dir_all(plugins.join("sample")).unwrap();
        let python = root.join("python");
        let uv = root.join("uv");
        fs::write(&python, b"python").unwrap();
        fs::write(&uv, b"uv").unwrap();
        symlink(&python, plugins.join("sample/plugin.py")).unwrap();
        assert!(matches!(
            InstallSourcePin::capture(&plugins, vec!["sample".into()], &python, &uv),
            Err(InstallSourceError::Unsafe(_))
        ));
        fs::remove_file(plugins.join("sample/plugin.py")).unwrap();
        let f = File::create(plugins.join("sample/large.bin")).unwrap();
        f.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(matches!(
            InstallSourcePin::capture(&plugins, vec!["sample".into()], &python, &uv),
            Err(InstallSourceError::Unsafe(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
