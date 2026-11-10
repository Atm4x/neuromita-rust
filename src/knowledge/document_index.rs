//! Explicit character-scoped, source-verified lexical document retrieval.
//! Text files remain authoritative; the index is always a disposable derivative.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::CharacterId;

use super::{
    memory_context::tokens,
    retrieval_fusion::{
        FusedHit, FusionConfig, FusionError, FusionMode, RetrievalBucket, RetrievalHit,
        RetrievalKey, RetrievalMethod, RetrievalSource, fuse,
    },
};

const FORMAT: &str = "neuromita-character-documents-index";
const VERSION: u32 = 1;
const MAX_FILES: usize = 48;
const MAX_SINGLE_BYTES: u64 = 128 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024;
const MAX_INDEX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHUNKS: usize = 1024;
const CHUNK_CHARS: usize = 800;
const CHUNK_STRIDE: usize = 680;

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("unsafe or unsupported document path: {0}")]
    UnsafePath(String),
    #[error("document input exceeds bounded capacity")]
    Limit,
    #[error("document contents are not valid UTF-8")]
    InvalidText,
    #[error("unsupported or invalid document index")]
    InvalidIndex,
    #[error("document source changed since indexing")]
    StaleIndex,
    #[error("document index belongs to a different character or root")]
    WrongScope,
    #[error("query must contain searchable tokens and fit within 2048 characters")]
    InvalidQuery,
    #[error("result count must be within 1..=20")]
    InvalidLimit,
    #[error("document index publication occurred, but directory durability is uncertain: {0}")]
    PublicationUncertain(String),
    #[error(transparent)]
    Fusion(#[from] FusionError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentEntry {
    pub path: String,
    pub source_sha256: String,
    pub bytes: u64,
    pub chunks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentIndex {
    pub format: String,
    pub version: u32,
    pub character: CharacterId,
    pub source_root: String,
    pub documents: Vec<DocumentEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStatus {
    EmptyCorpus,
    NoMatches,
    Selected,
}

pub struct DocumentSearch {
    pub status: SearchStatus,
    pub documents: usize,
    pub hits: Vec<FusedHit>,
}

fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn regular(path: &Path) -> Result<fs::Metadata, DocumentError> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(DocumentError::UnsafePath(path.display().to_string()));
    }
    Ok(meta)
}

#[cfg(unix)]
fn unchanged(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

#[cfg(not(unix))]
fn unchanged(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

fn read_regular(path: &Path, max: u64) -> Result<Vec<u8>, DocumentError> {
    let before = regular(path)?;
    if before.len() > max {
        return Err(DocumentError::Limit);
    }
    let file = File::open(path)?;
    let opened = file.metadata()?;
    if !unchanged(&before, &opened) {
        return Err(DocumentError::StaleIndex);
    }
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(DocumentError::Limit);
    }
    if !unchanged(&opened, &file_metadata(path)?) {
        return Err(DocumentError::StaleIndex);
    }
    Ok(bytes)
}

fn file_metadata(path: &Path) -> Result<fs::Metadata, DocumentError> {
    regular(path)
}

fn validate_root(root: &Path) -> Result<(), DocumentError> {
    if !root.is_absolute() || fs::canonicalize(root)? != root {
        return Err(DocumentError::UnsafePath(root.display().to_string()));
    }
    let meta = fs::symlink_metadata(root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(DocumentError::UnsafePath(root.display().to_string()));
    }
    Ok(())
}

fn chunks(content: &str) -> Result<Vec<String>, DocumentError> {
    let chars = content.chars().collect::<Vec<_>>();
    let mut result = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + CHUNK_CHARS).min(chars.len());
        let text = chars[start..end].iter().collect::<String>();
        if !text.trim().is_empty() {
            result.push(text);
        }
        if end == chars.len() {
            break;
        }
        if result.len() > MAX_CHUNKS {
            return Err(DocumentError::Limit);
        }
        start += CHUNK_STRIDE;
    }
    Ok(result)
}

pub fn inspect_source(root: &Path, character: CharacterId) -> Result<DocumentIndex, DocumentError> {
    validate_root(root)?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| DocumentError::UnsafePath(entry.path().display().to_string()))?;
        if !(name.ends_with(".md") || name.ends_with(".txt")) {
            continue;
        }
        if name.starts_with('.') || name.chars().count() > 128 || name.contains(['/', '\\']) {
            return Err(DocumentError::UnsafePath(
                entry.path().display().to_string(),
            ));
        }
        regular(&entry.path())?;
        paths.push(name.to_owned());
    }
    paths.sort();
    if paths.len() > MAX_FILES {
        return Err(DocumentError::Limit);
    }
    let mut total = 0u64;
    let mut documents = Vec::with_capacity(paths.len());
    let mut chunk_count = 0usize;
    for name in paths {
        let path = root.join(&name);
        let bytes = read_regular(&path, MAX_SINGLE_BYTES)?;
        total += bytes.len() as u64;
        if total > MAX_TOTAL_BYTES {
            return Err(DocumentError::Limit);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| DocumentError::InvalidText)?;
        let pieces = chunks(text)?;
        chunk_count += pieces.len();
        if chunk_count > MAX_CHUNKS {
            return Err(DocumentError::Limit);
        }
        documents.push(DocumentEntry {
            path: name,
            source_sha256: sha256(&bytes),
            bytes: bytes.len() as u64,
            chunks: pieces,
        });
    }
    Ok(DocumentIndex {
        format: FORMAT.into(),
        version: VERSION,
        character,
        source_root: root
            .to_str()
            .ok_or_else(|| DocumentError::UnsafePath(root.display().to_string()))?
            .into(),
        documents,
    })
}

fn validate_index(
    index: &DocumentIndex,
    root: &Path,
    character: CharacterId,
) -> Result<(), DocumentError> {
    if index.format != FORMAT || index.version != VERSION {
        return Err(DocumentError::InvalidIndex);
    }
    if index.character != character || Path::new(&index.source_root) != root {
        return Err(DocumentError::WrongScope);
    }
    let actual = inspect_source(root, character)?;
    if *index != actual {
        return Err(DocumentError::StaleIndex);
    }
    Ok(())
}

pub fn load_verified(
    root: &Path,
    character: CharacterId,
    index_path: &Path,
) -> Result<DocumentIndex, DocumentError> {
    let bytes = read_regular(index_path, MAX_INDEX_BYTES)?;
    let index: DocumentIndex =
        serde_json::from_slice(&bytes).map_err(|_| DocumentError::InvalidIndex)?;
    validate_index(&index, root, character)?;
    Ok(index)
}

/// The authoritative corpus is owned by the caller's existing character directory.
/// Import is explicit and never rebuilds or activates any derived index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedDocument {
    pub character: CharacterId,
    pub name: String,
    pub source_sha256: String,
    pub bytes: usize,
    pub requires_reindex: bool,
}

pub fn import_new_document(
    root: &Path,
    character: CharacterId,
    source: &Path,
    name: &str,
) -> Result<ImportedDocument, DocumentError> {
    validate_root(root)?;
    if !source.is_absolute() || fs::canonicalize(source)? != source || source.starts_with(root) {
        return Err(DocumentError::UnsafePath(source.display().to_string()));
    }
    if name.starts_with('.')
        || name.chars().count() > 128
        || name.is_empty()
        || name.trim() != name
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\'])
        || !(name.ends_with(".md") || name.ends_with(".txt"))
    {
        return Err(DocumentError::UnsafePath(name.to_owned()));
    }
    let destination = root.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(DocumentError::UnsafePath(destination.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(DocumentError::Io(error)),
    }
    let before = inspect_source(root, character)?;
    if before.documents.len() >= MAX_FILES {
        return Err(DocumentError::Limit);
    }
    let bytes = read_regular(source, MAX_SINGLE_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| DocumentError::InvalidText)?;
    let added_chunks = chunks(text)?;
    if added_chunks.is_empty()
        || before
            .documents
            .iter()
            .map(|d| d.chunks.len())
            .sum::<usize>()
            + added_chunks.len()
            > MAX_CHUNKS
        || before.documents.iter().map(|d| d.bytes).sum::<u64>() + bytes.len() as u64
            > MAX_TOTAL_BYTES
    {
        return Err(DocumentError::Limit);
    }
    let stage = root.join(format!(
        ".neuromita-document-import-{}",
        uuid::Uuid::new_v4()
    ));
    let publication = (|| -> Result<(), DocumentError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&stage)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if inspect_source(root, character)? != before
            || read_regular(source, MAX_SINGLE_BYTES)? != bytes
        {
            return Err(DocumentError::StaleIndex);
        }
        fs::hard_link(&stage, &destination)?;
        #[cfg(unix)]
        File::open(root)
            .and_then(|handle| handle.sync_all())
            .map_err(|error| DocumentError::PublicationUncertain(error.to_string()))?;
        Ok(())
    })();
    let _ = fs::remove_file(&stage);
    publication?;
    Ok(ImportedDocument {
        character,
        name: name.to_owned(),
        source_sha256: sha256(&bytes),
        bytes: bytes.len(),
        requires_reindex: true,
    })
}

/// Create an independently published canonical corpus revision with one named document
/// replaced. Never rename, unlink, or rewrite a byte in the previous corpus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForkedCorpus {
    pub character: CharacterId,
    pub replaced_name: String,
    pub previous_sha256: String,
    pub replacement_sha256: String,
    pub output_root: String,
    pub requires_reindex: bool,
}

fn rename_directory_without_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(from.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid staging path")
        })?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid output path")
        })?;
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let from = from
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let to = to
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (from, to);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no-clobber directory rename unsupported",
        ))
    }
}

fn write_private_document(path: &Path, bytes: &[u8]) -> Result<(), DocumentError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// A versioned corpus mutation only publishes a fresh directory. Neither mutation
/// removes or overwrites a byte from the original corpus.
pub fn fork_replacing_document(
    root: &Path,
    character: CharacterId,
    name: &str,
    expected_sha256: &str,
    replacement: &Path,
    output_root: &Path,
) -> Result<ForkedCorpus, DocumentError> {
    fork_replacing_with_hooks(
        root,
        character,
        name,
        expected_sha256,
        replacement,
        output_root,
        || Ok(()),
        sync_corpus_parent,
    )
}

/// The removed document remains available in the original, unchanged corpus.
/// The new corpus is a candidate until an operator explicitly reindexes and
/// activates it via the existing settings revision CAS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForkedCorpusRemoval {
    pub character: CharacterId,
    pub removed_name: String,
    pub previous_sha256: String,
    pub output_root: String,
    pub remaining_documents: usize,
    pub requires_reindex: bool,
}

/// A new versioned corpus includes one additional verified document. The old
/// corpus and its selected index remain untouched until explicit activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForkedCorpusAddition {
    pub character: CharacterId,
    pub added_name: String,
    pub source_sha256: String,
    pub output_root: String,
    pub total_documents: usize,
    pub requires_reindex: bool,
}

pub fn fork_adding_document(
    root: &Path,
    character: CharacterId,
    name: &str,
    source: &Path,
    output_root: &Path,
) -> Result<ForkedCorpusAddition, DocumentError> {
    fork_adding_with_hooks(
        root,
        character,
        name,
        source,
        output_root,
        || Ok(()),
        sync_corpus_parent,
    )
}

pub fn fork_removing_document(
    root: &Path,
    character: CharacterId,
    name: &str,
    expected_sha256: &str,
    output_root: &Path,
) -> Result<ForkedCorpusRemoval, DocumentError> {
    fork_removing_with_hooks(
        root,
        character,
        name,
        expected_sha256,
        output_root,
        || Ok(()),
        sync_corpus_parent,
    )
}

fn sync_corpus_parent(parent: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

fn fork_replacing_with_hooks(
    root: &Path,
    character: CharacterId,
    name: &str,
    expected_sha256: &str,
    replacement: &Path,
    output_root: &Path,
    after_staging: impl FnOnce() -> Result<(), DocumentError>,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<ForkedCorpus, DocumentError> {
    let result = fork_changing_with_hooks(
        root,
        character,
        name,
        ForkChange::Replace {
            expected_sha256,
            source: replacement,
        },
        output_root,
        after_staging,
        sync_parent,
    )?;
    Ok(ForkedCorpus {
        character,
        replaced_name: name.to_owned(),
        previous_sha256: expected_sha256.to_owned(),
        replacement_sha256: result
            .replacement_sha256
            .ok_or(DocumentError::InvalidText)?,
        output_root: result.output_root,
        requires_reindex: true,
    })
}

fn fork_removing_with_hooks(
    root: &Path,
    character: CharacterId,
    name: &str,
    expected_sha256: &str,
    output_root: &Path,
    after_staging: impl FnOnce() -> Result<(), DocumentError>,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<ForkedCorpusRemoval, DocumentError> {
    let result = fork_changing_with_hooks(
        root,
        character,
        name,
        ForkChange::Remove { expected_sha256 },
        output_root,
        after_staging,
        sync_parent,
    )?;
    Ok(ForkedCorpusRemoval {
        character,
        removed_name: name.to_owned(),
        previous_sha256: expected_sha256.to_owned(),
        output_root: result.output_root,
        remaining_documents: result.document_count,
        requires_reindex: true,
    })
}

fn fork_adding_with_hooks(
    root: &Path,
    character: CharacterId,
    name: &str,
    source: &Path,
    output_root: &Path,
    after_staging: impl FnOnce() -> Result<(), DocumentError>,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<ForkedCorpusAddition, DocumentError> {
    let result = fork_changing_with_hooks(
        root,
        character,
        name,
        ForkChange::Add { source },
        output_root,
        after_staging,
        sync_parent,
    )?;
    Ok(ForkedCorpusAddition {
        character,
        added_name: name.to_owned(),
        source_sha256: result
            .replacement_sha256
            .ok_or(DocumentError::InvalidText)?,
        output_root: result.output_root,
        total_documents: result.document_count,
        requires_reindex: true,
    })
}

#[derive(Clone, Copy)]
enum ForkChange<'a> {
    Add {
        source: &'a Path,
    },
    Replace {
        expected_sha256: &'a str,
        source: &'a Path,
    },
    Remove {
        expected_sha256: &'a str,
    },
}

impl<'a> ForkChange<'a> {
    fn source(self) -> Option<&'a Path> {
        match self {
            Self::Add { source } | Self::Replace { source, .. } => Some(source),
            Self::Remove { .. } => None,
        }
    }

    fn expected_sha256(self) -> Option<&'a str> {
        match self {
            Self::Replace {
                expected_sha256, ..
            }
            | Self::Remove { expected_sha256 } => Some(expected_sha256),
            Self::Add { .. } => None,
        }
    }
}

struct ForkOutcome {
    output_root: String,
    replacement_sha256: Option<String>,
    document_count: usize,
}

fn inspect_owned_corpus(
    root: &Path,
    character: CharacterId,
) -> Result<DocumentIndex, DocumentError> {
    let corpus = inspect_source(root, character)?;
    let entry_count = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?.len();
    if entry_count != corpus.documents.len() {
        return Err(DocumentError::UnsafePath(
            "corpus contains entries outside the .md/.txt ownership contract".into(),
        ));
    }
    Ok(corpus)
}

fn fork_changing_with_hooks(
    root: &Path,
    character: CharacterId,
    name: &str,
    change: ForkChange<'_>,
    output_root: &Path,
    after_staging: impl FnOnce() -> Result<(), DocumentError>,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<ForkOutcome, DocumentError> {
    validate_root(root)?;
    if name.is_empty()
        || name.starts_with('.')
        || name.trim() != name
        || name.chars().count() > 128
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\'])
        || !(name.ends_with(".md") || name.ends_with(".txt"))
        || change.expected_sha256().is_some_and(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(DocumentError::UnsafePath(
            "invalid document name or expected digest".into(),
        ));
    }
    if !output_root.is_absolute() || output_root.starts_with(root) {
        return Err(DocumentError::UnsafePath("invalid output location".into()));
    }
    let parent = output_root
        .parent()
        .ok_or_else(|| DocumentError::UnsafePath("output needs parent".into()))?;
    validate_root(parent)?;
    match fs::symlink_metadata(output_root) {
        Ok(_) => return Err(DocumentError::UnsafePath("output already exists".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let output_root_string = output_root
        .to_str()
        .ok_or_else(|| DocumentError::UnsafePath("output must be UTF-8".into()))?
        .to_owned();
    let original = inspect_owned_corpus(root, character)?;
    match change.expected_sha256() {
        Some(expected_sha256) => {
            let entry = original
                .documents
                .iter()
                .find(|item| item.path == name)
                .ok_or_else(|| {
                    DocumentError::UnsafePath("document not found in the canonical corpus".into())
                })?;
            if entry.source_sha256 != expected_sha256 {
                return Err(DocumentError::StaleIndex);
            }
        }
        None => {
            if original.documents.len() >= MAX_FILES {
                return Err(DocumentError::Limit);
            }
            if original.documents.iter().any(|item| item.path == name) {
                return Err(DocumentError::UnsafePath(
                    "document name already exists".into(),
                ));
            }
        }
    }
    let replacement_bytes = change
        .source()
        .map(|path| {
            if !path.is_absolute() || fs::canonicalize(path)? != path || path.starts_with(root) {
                return Err(DocumentError::UnsafePath(
                    "invalid document input path".into(),
                ));
            }
            let bytes = read_regular(path, MAX_SINGLE_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| DocumentError::InvalidText)?;
            if chunks(text)?.is_empty() {
                return Err(DocumentError::InvalidText);
            }
            Ok(bytes)
        })
        .transpose()?;
    let replacement_sha256 = replacement_bytes.as_ref().map(|bytes| sha256(bytes));
    let stage = parent.join(format!(".neuromita-document-fork-{}", uuid::Uuid::new_v4()));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&stage)?;
    let mut published = false;
    let publication = (|| -> Result<usize, DocumentError> {
        for document in &original.documents {
            let data = if document.path == name {
                match change {
                    ForkChange::Replace { .. } => replacement_bytes
                        .as_ref()
                        .ok_or(DocumentError::InvalidText)?
                        .clone(),
                    ForkChange::Remove { .. } => continue,
                    ForkChange::Add { .. } => {
                        read_regular(&root.join(&document.path), MAX_SINGLE_BYTES)?
                    }
                }
            } else {
                read_regular(&root.join(&document.path), MAX_SINGLE_BYTES)?
            };
            if !(matches!(change, ForkChange::Replace { .. }) && document.path == name)
                && sha256(&data) != document.source_sha256
            {
                return Err(DocumentError::StaleIndex);
            }
            write_private_document(&stage.join(&document.path), &data)?;
        }
        if matches!(change, ForkChange::Add { .. }) {
            write_private_document(
                &stage.join(name),
                replacement_bytes
                    .as_ref()
                    .ok_or(DocumentError::InvalidText)?,
            )?;
        }
        let next = inspect_source(&stage, character)?;
        let mut expected = original.documents.clone();
        match change {
            ForkChange::Remove { .. } => expected.retain(|document| document.path != name),
            ForkChange::Replace { .. } | ForkChange::Add { .. } => {
                let bytes = replacement_bytes
                    .as_ref()
                    .ok_or(DocumentError::InvalidText)?;
                let new_entry = DocumentEntry {
                    path: name.to_owned(),
                    source_sha256: replacement_sha256
                        .clone()
                        .ok_or(DocumentError::InvalidText)?,
                    bytes: bytes.len() as u64,
                    chunks: chunks(
                        std::str::from_utf8(bytes).map_err(|_| DocumentError::InvalidText)?,
                    )?,
                };
                if matches!(change, ForkChange::Add { .. }) {
                    expected.push(new_entry);
                    expected.sort_by(|a, b| a.path.cmp(&b.path));
                } else {
                    *expected
                        .iter_mut()
                        .find(|document| document.path == name)
                        .ok_or(DocumentError::StaleIndex)? = new_entry;
                }
            }
        }
        if next.documents != expected {
            return Err(DocumentError::StaleIndex);
        }
        after_staging()?;
        if inspect_owned_corpus(root, character)? != original
            || change
                .source()
                .zip(replacement_bytes.as_ref())
                .map(|(path, bytes)| {
                    read_regular(path, MAX_SINGLE_BYTES).map(|current| current != *bytes)
                })
                .transpose()?
                .unwrap_or(false)
            || inspect_source(&stage, character)?.documents != next.documents
        {
            return Err(DocumentError::StaleIndex);
        }
        #[cfg(unix)]
        File::open(&stage)?.sync_all()?;
        rename_directory_without_replace(&stage, output_root)?;
        published = true;
        sync_parent(parent).map_err(|error| {
            DocumentError::PublicationUncertain(format!("{}: {error}", output_root.display()))
        })?;
        Ok(next.documents.len())
    })();
    if !published {
        let _ = fs::remove_dir_all(&stage);
    }
    let document_count = publication?;
    Ok(ForkOutcome {
        output_root: output_root_string,
        replacement_sha256,
        document_count,
    })
}

pub fn build_new(
    root: &Path,
    character: CharacterId,
    destination: &Path,
) -> Result<DocumentIndex, DocumentError> {
    validate_root(root)?;
    if !destination.is_absolute() || destination.starts_with(root) || destination == root {
        return Err(DocumentError::UnsafePath(destination.display().to_string()));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| DocumentError::UnsafePath(destination.display().to_string()))?;
    validate_root(parent)?;
    if fs::symlink_metadata(destination).is_ok() {
        return Err(DocumentError::UnsafePath(destination.display().to_string()));
    }
    let index = inspect_source(root, character)?;
    let mut contents =
        serde_json::to_vec_pretty(&index).map_err(|_| DocumentError::InvalidIndex)?;
    contents.push(b'\n');
    if contents.len() as u64 > MAX_INDEX_BYTES {
        return Err(DocumentError::Limit);
    }
    let stage = parent.join(format!(
        ".neuromita-document-index-{}",
        uuid::Uuid::new_v4()
    ));
    let publication = (|| -> Result<(), DocumentError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&stage)?;
        file.write_all(&contents)?;
        file.sync_all()?;
        if inspect_source(root, character)? != index {
            return Err(DocumentError::StaleIndex);
        }
        fs::hard_link(&stage, destination)?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|handle| handle.sync_all())
            .map_err(|err| DocumentError::PublicationUncertain(err.to_string()))?;
        Ok(())
    })();
    let _ = fs::remove_file(&stage);
    publication?;
    Ok(index)
}

/// Create one immutable, uniquely named derived index revision without selecting it.
/// The caller must explicitly activate the returned path through the settings owner.
pub fn build_new_revision(
    root: &Path,
    character: CharacterId,
    revisions_directory: &Path,
) -> Result<(std::path::PathBuf, DocumentIndex), DocumentError> {
    validate_root(revisions_directory)?;
    if revisions_directory.starts_with(root) || revisions_directory == root {
        return Err(DocumentError::UnsafePath(
            revisions_directory.display().to_string(),
        ));
    }
    let destination = revisions_directory.join(format!(
        "{}-{}.json",
        character.as_str(),
        uuid::Uuid::new_v4()
    ));
    let index = build_new(root, character, &destination)?;
    load_verified(root, character, &destination)?;
    Ok((destination, index))
}

pub fn search_verified(
    root: &Path,
    character: CharacterId,
    index_path: &Path,
    query: &str,
    limit: usize,
) -> Result<DocumentSearch, DocumentError> {
    if limit == 0 || limit > 20 {
        return Err(DocumentError::InvalidLimit);
    }
    if query.chars().count() > 2048 || tokens(query).is_empty() {
        return Err(DocumentError::InvalidQuery);
    }
    let index = load_verified(root, character, index_path)?;
    let terms = tokens(query);
    let mut ranked = Vec::new();
    for document in &index.documents {
        for (chunk_index, content) in document.chunks.iter().enumerate() {
            let score = tokens(content).intersection(&terms).count();
            if score == 0 {
                continue;
            }
            let mut metadata = BTreeMap::new();
            metadata.insert("source_path".into(), document.path.clone());
            metadata.insert("source_sha256".into(), document.source_sha256.clone());
            metadata.insert("chunk_index".into(), chunk_index.to_string());
            ranked.push((
                score,
                RetrievalHit {
                    key: RetrievalKey {
                        character,
                        source: RetrievalSource::Document,
                        id: format!("{}#{chunk_index}", document.path),
                    },
                    content: content.clone(),
                    raw_score: Some(score as f64),
                    metadata,
                },
            ));
        }
    }
    ranked.sort_by(|(a_score, a_hit), (b_score, b_hit)| {
        b_score.cmp(a_score).then_with(|| a_hit.key.cmp(&b_hit.key))
    });
    let hits = if ranked.is_empty() {
        Vec::new()
    } else {
        fuse(
            character,
            &[RetrievalBucket {
                method: RetrievalMethod::Keyword,
                hits: ranked.into_iter().take(1024).map(|(_, hit)| hit).collect(),
            }],
            FusionConfig {
                mode: FusionMode::Union,
                limit,
                minimum_graph: 0,
            },
        )?
    };
    let status = if index.documents.is_empty() {
        SearchStatus::EmptyCorpus
    } else if hits.is_empty() {
        SearchStatus::NoMatches
    } else {
        SearchStatus::Selected
    };
    Ok(DocumentSearch {
        status,
        documents: index.documents.len(),
        hits,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod fork_tests {
    use super::*;
    use std::io;

    struct Fixture {
        temp: std::path::PathBuf,
        root: std::path::PathBuf,
        replacement: std::path::PathBuf,
        target: std::path::PathBuf,
        expected: String,
    }

    impl Fixture {
        fn new() -> Self {
            let temp =
                std::env::temp_dir().join(format!("neuromita-fork-{}", uuid::Uuid::new_v4()));
            let root = temp.join("original");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("notes.md"), "The unchanged owner file").unwrap();
            let replacement = temp.join("updated.txt");
            fs::write(&replacement, "A new versioned document").unwrap();
            let target = temp.join("new-version");
            let expected = sha256(&fs::read(root.join("notes.md")).unwrap());
            Self {
                temp,
                root,
                replacement,
                target,
                expected,
            }
        }
        fn call(
            &self,
            hook: impl FnOnce() -> Result<(), DocumentError>,
            sync: impl FnOnce(&Path) -> io::Result<()>,
        ) -> Result<ForkedCorpus, DocumentError> {
            fork_replacing_with_hooks(
                &self.root,
                CharacterId::Kind,
                "notes.md",
                &self.expected,
                &self.replacement,
                &self.target,
                hook,
                sync,
            )
        }
        fn assert_no_staging(&self) {
            assert!(fs::read_dir(&self.temp).unwrap().all(|item| {
                !item
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".neuromita-document-fork-")
            }));
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.temp);
        }
    }

    #[test]
    fn addition_refuses_changed_source_before_publication() {
        let f = Fixture::new();
        let result = fork_adding_with_hooks(
            &f.root,
            CharacterId::Kind,
            "added.md",
            &f.replacement,
            &f.target,
            || {
                fs::write(&f.replacement, "replaced while staging")?;
                Ok(())
            },
            |parent| File::open(parent)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::StaleIndex)));
        assert!(!f.target.exists());
        assert!(!f.root.join("added.md").exists());
        f.assert_no_staging();
    }

    #[test]
    fn addition_does_not_overwrite_concurrent_destination() {
        let f = Fixture::new();
        let result = fork_adding_with_hooks(
            &f.root,
            CharacterId::Kind,
            "added.md",
            &f.replacement,
            &f.target,
            || {
                fs::create_dir(&f.target)?;
                fs::write(f.target.join("foreign.md"), "foreign winner")?;
                Ok(())
            },
            |parent| File::open(parent)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::Io(_))));
        assert_eq!(
            fs::read_to_string(f.target.join("foreign.md")).unwrap(),
            "foreign winner"
        );
        f.assert_no_staging();
    }

    #[test]
    fn addition_rejects_foreign_file_introduced_while_staging() {
        let f = Fixture::new();
        let result = fork_adding_with_hooks(
            &f.root,
            CharacterId::Kind,
            "added.md",
            &f.replacement,
            &f.target,
            || {
                fs::write(f.root.join("late-foreign.bin"), "unowned")?;
                Ok(())
            },
            |parent| File::open(parent)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::UnsafePath(_))));
        assert!(!f.target.exists());
        assert_eq!(
            fs::read_to_string(f.root.join("late-foreign.bin")).unwrap(),
            "unowned"
        );
        f.assert_no_staging();
    }

    #[test]
    fn addition_postpublication_sync_failure_keeps_candidate() {
        let f = Fixture::new();
        let result = fork_adding_with_hooks(
            &f.root,
            CharacterId::Kind,
            "added.md",
            &f.replacement,
            &f.target,
            || Ok(()),
            |_| Err(io::Error::other("injected parent sync error")),
        );
        assert!(matches!(
            result,
            Err(DocumentError::PublicationUncertain(_))
        ));
        assert_eq!(
            fs::read(f.target.join("added.md")).unwrap(),
            fs::read(&f.replacement).unwrap()
        );
        assert!(f.root.join("notes.md").exists());
        f.assert_no_staging();
    }

    #[test]
    fn removal_rejects_changed_source_before_publishing_and_cleans_staging() {
        let f = Fixture::new();
        let result = fork_removing_with_hooks(
            &f.root,
            CharacterId::Kind,
            "notes.md",
            &f.expected,
            &f.target,
            || {
                fs::write(f.root.join("notes.md"), "Owner updated file")?;
                Ok(())
            },
            |parent| File::open(parent)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::StaleIndex)));
        assert!(!f.target.exists());
        assert_eq!(
            fs::read_to_string(f.root.join("notes.md")).unwrap(),
            "Owner updated file"
        );
        f.assert_no_staging();
    }

    #[test]
    fn removal_does_not_overwrite_foreign_destination() {
        let f = Fixture::new();
        let result = fork_removing_with_hooks(
            &f.root,
            CharacterId::Kind,
            "notes.md",
            &f.expected,
            &f.target,
            || {
                fs::create_dir(&f.target)?;
                fs::write(f.target.join("foreign.md"), "foreign")?;
                Ok(())
            },
            |parent| File::open(parent)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::Io(_))));
        assert_eq!(
            fs::read_to_string(f.target.join("foreign.md")).unwrap(),
            "foreign"
        );
        f.assert_no_staging();
    }

    #[test]
    fn removal_postpublication_sync_failure_preserves_candidate() {
        let f = Fixture::new();
        let result = fork_removing_with_hooks(
            &f.root,
            CharacterId::Kind,
            "notes.md",
            &f.expected,
            &f.target,
            || Ok(()),
            |_| Err(io::Error::other("forced directory fsync error")),
        );
        assert!(matches!(
            result,
            Err(DocumentError::PublicationUncertain(_))
        ));
        assert!(!f.target.join("notes.md").exists());
        assert!(f.root.join("notes.md").exists());
        f.assert_no_staging();
    }

    #[test]
    fn concurrent_old_root_replacement_fails_before_publishing_new_root() {
        let f = Fixture::new();
        let result = f.call(
            || {
                fs::write(
                    f.root.join("notes.md"),
                    "Someone else updated this document",
                )?;
                Ok(())
            },
            |dir| File::open(dir)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::StaleIndex)));
        assert!(!f.target.exists());
        assert_eq!(
            fs::read_to_string(f.root.join("notes.md")).unwrap(),
            "Someone else updated this document"
        );
        f.assert_no_staging();
    }

    #[test]
    fn destination_appearing_during_staging_is_preserved() {
        let f = Fixture::new();
        let result = f.call(
            || {
                fs::create_dir(&f.target)?;
                fs::write(f.target.join("foreign.md"), "foreign winner")?;
                Ok(())
            },
            |dir| File::open(dir)?.sync_all(),
        );
        assert!(matches!(result, Err(DocumentError::Io(_))));
        assert_eq!(
            fs::read_to_string(f.target.join("foreign.md")).unwrap(),
            "foreign winner"
        );
        assert_eq!(
            fs::read_to_string(f.root.join("notes.md")).unwrap(),
            "The unchanged owner file"
        );
        f.assert_no_staging();
    }

    #[test]
    fn post_publication_sync_failure_does_not_delete_published_version() {
        let f = Fixture::new();
        let result = f.call(
            || Ok(()),
            |_| Err(io::Error::other("forced parent fsync failure")),
        );
        assert!(matches!(
            result,
            Err(DocumentError::PublicationUncertain(_))
        ));
        assert_eq!(
            fs::read_to_string(f.target.join("notes.md")).unwrap(),
            "A new versioned document"
        );
        assert_eq!(
            fs::read_to_string(f.root.join("notes.md")).unwrap(),
            "The unchanged owner file"
        );
        f.assert_no_staging();
    }
}
