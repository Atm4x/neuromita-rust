//! Read-only preflight for the historical Python ZIP file inventory.
//!
//! This is not a ZIP decoder, extractor, or installation authorization. An
//! eventual reviewed extractor must supply every directory entry, including
//! entry type and uncompressed size, before writing any extracted bytes.

use std::collections::HashMap;

use thiserror::Error;

const MAX_ENTRIES: usize = 20_000;
const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DEPTH: usize = 20;
const MAX_PATH_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: String,
    pub kind: ArchiveEntryKind,
    pub uncompressed_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOwnership {
    Application,
    PreserveUser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    relative_path: String,
    size: u64,
    ownership: FileOwnership,
}

impl PlannedFile {
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn ownership(&self) -> FileOwnership {
        self.ownership
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonArchivePreflight {
    files: Vec<PlannedFile>,
    application_bytes: u64,
    preserved_bytes: u64,
}

impl PythonArchivePreflight {
    pub fn files(&self) -> &[PlannedFile] {
        &self.files
    }

    pub fn application_bytes(&self) -> u64 {
        self.application_bytes
    }

    pub fn preserved_bytes(&self) -> u64 {
        self.preserved_bytes
    }

    pub fn preserved_files(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.ownership == FileOwnership::PreserveUser)
            .count()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PreflightError {
    #[error("the package contains too many entries or too much uncompressed data")]
    ResourceLimit,
    #[error("the package has an unsafe relative path: {0}")]
    UnsafePath(String),
    #[error("the package contains an unsupported entry type: {0}")]
    UnsupportedEntry(String),
    #[error("the package contains an unrecognized installation path: {0}")]
    UnownedPath(String),
    #[error("the package contains colliding paths: {0}")]
    PathCollision(String),
    #[error("the package has a file/directory hierarchy conflict: {0}")]
    HierarchyConflict(String),
    #[error("the package is missing a required application file: {0}")]
    MissingRequired(&'static str),
}

/// Classify a fully enumerated, already publisher-verified Python ZIP before
/// any extraction. This read-only result must not be treated as permission to
/// apply a package or overwrite existing installation paths.
pub fn preflight_python_archive(
    entries: impl IntoIterator<Item = ArchiveEntry>,
) -> Result<PythonArchivePreflight, PreflightError> {
    let mut known: HashMap<String, ArchiveEntryKind> = HashMap::new();
    let mut files = Vec::new();
    let mut application_bytes = 0u64;
    let mut preserved_bytes = 0u64;
    let mut total_bytes = 0u64;
    let mut count = 0usize;

    for entry in entries {
        count += 1;
        if count > MAX_ENTRIES {
            return Err(PreflightError::ResourceLimit);
        }
        let path = checked_relative_path(&entry.path, entry.kind)?;
        let folded = path.to_ascii_lowercase();
        if known.insert(folded.clone(), entry.kind).is_some() {
            return Err(PreflightError::PathCollision(path.to_owned()));
        }
        if matches!(
            entry.kind,
            ArchiveEntryKind::Symlink | ArchiveEntryKind::Other
        ) {
            return Err(PreflightError::UnsupportedEntry(path.to_owned()));
        }
        let ownership = classify_python_path(path)?;
        if entry.kind == ArchiveEntryKind::File
            && !path.contains('/')
            && matches!(path, "Settings" | "Prompts" | "assets" | "docs" | "libs")
        {
            return Err(PreflightError::UnsupportedEntry(path.to_owned()));
        }
        if entry.kind == ArchiveEntryKind::Directory {
            if entry.uncompressed_bytes != 0 {
                return Err(PreflightError::UnsupportedEntry(path.to_owned()));
            }
            continue;
        }
        if entry.uncompressed_bytes > MAX_FILE_BYTES {
            return Err(PreflightError::ResourceLimit);
        }
        total_bytes = total_bytes
            .checked_add(entry.uncompressed_bytes)
            .ok_or(PreflightError::ResourceLimit)?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(PreflightError::ResourceLimit);
        }
        match ownership {
            FileOwnership::Application => application_bytes += entry.uncompressed_bytes,
            FileOwnership::PreserveUser => preserved_bytes += entry.uncompressed_bytes,
        }
        files.push(PlannedFile {
            relative_path: path.to_owned(),
            size: entry.uncompressed_bytes,
            ownership,
        });
    }

    for path in known.keys() {
        let mut prefix = String::new();
        let components = path.split('/').collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate().take(components.len() - 1) {
            if index > 0 {
                prefix.push('/');
            }
            prefix.push_str(component);
            if known.get(&prefix.to_ascii_lowercase()) == Some(&ArchiveEntryKind::File) {
                return Err(PreflightError::HierarchyConflict(prefix));
            }
        }
    }
    for required in ["Launcher.exe", "NeuroMita.pyz"] {
        if !files.iter().any(|file| {
            file.relative_path == required
                && file.ownership == FileOwnership::Application
                && file.size > 0
        }) {
            return Err(PreflightError::MissingRequired(required));
        }
    }
    files.sort_unstable_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(PythonArchivePreflight {
        files,
        application_bytes,
        preserved_bytes,
    })
}

fn checked_relative_path(path: &str, kind: ArchiveEntryKind) -> Result<&str, PreflightError> {
    let normalized = if kind == ArchiveEntryKind::Directory {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    if normalized.is_empty()
        || normalized.len() > MAX_PATH_BYTES
        || path.contains('\\')
        || path.contains(':')
        || path.contains('\0')
        || kind != ArchiveEntryKind::Directory && path.ends_with('/')
    {
        return Err(PreflightError::UnsafePath(path.to_owned()));
    }
    let components = normalized.split('/').collect::<Vec<_>>();
    if components.len() > MAX_DEPTH
        || components.iter().any(|part| {
            part.is_empty()
                || part.len() > 255
                || part == &"."
                || part == &".."
                || part.ends_with([' ', '.'])
                || part.starts_with(' ')
                || part.bytes().any(|b| b.is_ascii_control())
                || part.chars().any(|c| !c.is_ascii())
                || windows_reserved_name(part)
        })
    {
        return Err(PreflightError::UnsafePath(path.to_owned()));
    }
    Ok(normalized)
}

fn windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.len() == 4
            && matches!(&stem[..3], "COM" | "LPT")
            && matches!(stem.as_bytes()[3], b'1'..=b'9')
}

fn classify_python_path(path: &str) -> Result<FileOwnership, PreflightError> {
    let root = path.split('/').next().unwrap_or(path);
    if root == "Settings" || root == "Prompts" {
        return Ok(FileOwnership::PreserveUser);
    }
    let allowed_directory = matches!(root, "assets" | "docs" | "libs");
    let allowed_root_file = !path.contains('/')
        && matches!(
            root,
            "Launcher.exe"
                | "NeuroMita.pyz"
                | "NM_Logo.png"
                | "run.py"
                | "run.bat"
                | "requirements.txt"
        );
    if allowed_directory || allowed_root_file {
        Ok(FileOwnership::Application)
    } else {
        Err(PreflightError::UnownedPath(path.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> ArchiveEntry {
        ArchiveEntry {
            path: path.into(),
            kind: ArchiveEntryKind::File,
            uncompressed_bytes: size,
        }
    }

    fn directory(path: &str) -> ArchiveEntry {
        ArchiveEntry {
            path: path.into(),
            kind: ArchiveEntryKind::Directory,
            uncompressed_bytes: 0,
        }
    }

    fn base() -> Vec<ArchiveEntry> {
        vec![file("Launcher.exe", 42), file("NeuroMita.pyz", 55)]
    }

    #[test]
    fn real_python_layout_separates_user_owned_content() {
        let mut entries = base();
        entries.extend([
            directory("Settings/"),
            file("Settings/current.json", 123),
            file("Prompts/custom/user.txt", 45),
            file("assets/guide/guide_welcome.png", 100),
            file("docs/wiki/en/index.md", 60),
            file("libs/bridge.py", 20),
            file("run.py", 30),
        ]);
        let plan = preflight_python_archive(entries).unwrap();
        assert_eq!(plan.application_bytes(), 307);
        assert_eq!(plan.preserved_bytes(), 168);
        assert_eq!(plan.preserved_files(), 2);
        assert_eq!(plan.files().len(), 8);
        assert_eq!(plan.files()[0].relative_path(), "Launcher.exe");
        assert_eq!(plan.files()[0].ownership(), FileOwnership::Application);
        assert!(
            plan.files()
                .iter()
                .filter(|f| f.ownership() == FileOwnership::PreserveUser)
                .all(|f| f.relative_path().starts_with("Settings/")
                    || f.relative_path().starts_with("Prompts/"))
        );
    }

    #[test]
    fn rejects_traversal_absolute_ambiguous_and_windows_reserved_paths() {
        for invalid in [
            "../Settings/credentials.json",
            "/tmp/payload",
            "assets/../Settings/x",
            "assets//x",
            "assets/./x",
            "assets\\x",
            "assets/x:stream",
            "assets/CON.txt",
            "docs/LPT9.md",
            "assets/x. ",
            "assets/ ",
            "assets/кириллица.txt",
            "assets/x\n",
            "assets/x/",
            "./run.py",
        ] {
            let mut entries = base();
            entries.push(file(invalid, 1));
            assert!(
                matches!(
                    preflight_python_archive(entries),
                    Err(PreflightError::UnsafePath(_))
                ),
                "{invalid}"
            );
        }
    }

    #[test]
    fn unknown_owned_paths_are_rejected_not_silently_installed() {
        for name in [
            "UserData/private.db",
            ".neuromita-update/activation.json",
            "models/model.bin",
            "plugin.exe",
            "AIData/index.json",
        ] {
            let mut entries = base();
            entries.push(file(name, 1));
            assert_eq!(
                preflight_python_archive(entries),
                Err(PreflightError::UnownedPath(name.into()))
            );
        }
    }

    #[test]
    fn rejects_case_collisions_even_on_linux() {
        for name in ["launcher.EXE", "assets/FILE.png"] {
            let mut entries = base();
            if name.starts_with("assets") {
                entries.push(file("assets/file.png", 3));
            }
            entries.push(file(name, 1));
            assert!(matches!(
                preflight_python_archive(entries),
                Err(PreflightError::PathCollision(_))
            ));
        }
    }

    #[test]
    fn no_symlinks_special_entries_or_oversized_file() {
        for kind in [ArchiveEntryKind::Symlink, ArchiveEntryKind::Other] {
            let mut entries = base();
            entries.push(ArchiveEntry {
                path: "assets/outside".into(),
                kind,
                uncompressed_bytes: 0,
            });
            assert!(matches!(
                preflight_python_archive(entries),
                Err(PreflightError::UnsupportedEntry(_))
            ));
        }
        let mut entries = base();
        entries.push(file("assets/huge", MAX_FILE_BYTES + 1));
        assert_eq!(
            preflight_python_archive(entries),
            Err(PreflightError::ResourceLimit)
        );
    }

    #[test]
    fn missing_required_entry_fails_closed() {
        assert_eq!(
            preflight_python_archive([file("NeuroMita.pyz", 3)]),
            Err(PreflightError::MissingRequired("Launcher.exe"))
        );
        assert_eq!(
            preflight_python_archive([file("Launcher.exe", 3)]),
            Err(PreflightError::MissingRequired("NeuroMita.pyz"))
        );
        assert_eq!(
            preflight_python_archive([file("Launcher.exe", 3), file("NeuroMita.pyz", 0)]),
            Err(PreflightError::MissingRequired("NeuroMita.pyz"))
        );
    }

    #[test]
    fn rejects_file_as_parent_and_duplicate_file() {
        let mut entries = base();
        entries.push(file("assets/a", 1));
        entries.push(file("assets/a/nested.txt", 2));
        assert!(matches!(
            preflight_python_archive(entries),
            Err(PreflightError::HierarchyConflict(_))
        ));
        let mut entries = base();
        entries.push(file("Launcher.exe", 12));
        assert!(matches!(
            preflight_python_archive(entries),
            Err(PreflightError::PathCollision(_))
        ));
    }

    #[test]
    fn rejects_file_as_parent_of_explicit_directory() {
        let mut entries = base();
        entries.push(file("assets/a", 1));
        entries.push(directory("assets/a/child/"));
        assert!(matches!(
            preflight_python_archive(entries),
            Err(PreflightError::HierarchyConflict(_))
        ));
    }

    #[test]
    fn protected_and_application_directories_must_not_be_regular_files() {
        for name in ["Settings", "Prompts", "assets", "docs", "libs"] {
            let mut entries = base();
            entries.push(file(name, 10));
            assert_eq!(
                preflight_python_archive(entries),
                Err(PreflightError::UnsupportedEntry(name.into()))
            );
        }
    }

    #[test]
    fn entry_and_expansion_budgets_are_enforced() {
        let mut entries = base();
        entries.extend((0..MAX_ENTRIES).map(|index| file(&format!("assets/{index}.dat"), 1)));
        assert_eq!(
            preflight_python_archive(entries),
            Err(PreflightError::ResourceLimit)
        );
        let mut entries = base();
        entries.extend((0..9).map(|index| file(&format!("assets/{index}.dat"), MAX_FILE_BYTES)));
        assert_eq!(
            preflight_python_archive(entries),
            Err(PreflightError::ResourceLimit)
        );
    }

    #[test]
    fn malformed_directory_metadata_is_rejected() {
        let mut entries = base();
        entries.push(ArchiveEntry {
            path: "assets/".into(),
            kind: ArchiveEntryKind::Directory,
            uncompressed_bytes: 5,
        });
        assert!(matches!(
            preflight_python_archive(entries),
            Err(PreflightError::UnsupportedEntry(_))
        ));
    }
}
