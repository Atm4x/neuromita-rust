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
