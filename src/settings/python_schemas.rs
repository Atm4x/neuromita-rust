//! Conservative, read-only inventory of the Python APIModelSchemas source directory.
//! This does not install Python model schemas into active Rust settings.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

use super::python_presets::{parse_unique_json, sha256_hex};

const MAX_SCHEMAS: usize = 128;
const MAX_SCHEMA_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// Exact hashes from the original Python `bundled_json_compat.py` at
/// Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65.
/// SHA256 is over the validated schema object re-encoded as sorted-key,
/// compact UTF-8 JSON (`ensure_ascii=False`), never over raw file bytes.
const BUNDLED_FINGERPRINTS: &[(&str, &[&str])] = &[
    (
        "chatgpt-plan",
        &[
            "2f1ea5232f431e9eab8cb84e143baa2da1c7f9d5d01295f4418f880938a285f1",
            "5d1fbe5a685e999d38a15c6615140e71cc1b8cf236b021b89931fc6d2648ef98",
            "d42b6de1f9304fea199933e44a735b8dbdb7008a33e3bfb6755609999f726bb5",
        ],
    ),
    (
        "google-budget",
        &["3b8324b71335e993a7ab66cbd3557923d79a2c2fcd33d565edcd6fe4fd8de6ce"],
    ),
    (
        "google-budget-lite",
        &["e2678aa24afce60105436911dbce6b6451890f5740d91a87eab162256d3eccb2"],
    ),
    (
        "google-budget-pro",
        &["6a1fb73bb06e5a949937b3e115e1ec4d531fd6d89b912df62dddddefa8e663e1"],
    ),
    (
        "google-level",
        &["2fcce5c41b254dc3abb7ba0b5b6e2fcf158d3235f222a7b31ee2503a393b6302"],
    ),
    (
        "groq",
        &["4cef7da7caf01b558f2e16bccdaabd1380fb1adf24a32ad431cb9534c55adb1a"],
    ),
    (
        "local-openai",
        &["7c9f403d27a8d8101ab7cc94a826e2c44d2f1ceaaf0d1d707ee042657a7027f6"],
    ),
    (
        "mistral",
        &["aa8fdd5dc2468531ec27fb591ba5d1b16e5b9c74df3b4bb1bd5d2da0f03f7101"],
    ),
    (
        "openai-compatible",
        &["bcfc7f099fe31cdf6276e538d49a4d25d18bc8fdcbaba4abbe92ec214e53ee86"],
    ),
    (
        "openai-responses",
        &["e38f37fa1976f7d9b70f4460095d6bac5372cfc803f13a6c219b05d3226c2fec"],
    ),
    (
        "openrouter",
        &["770665964274fa05ef754a638b621908ab6af9ff6d03a8c08600c88811dbd470"],
    ),
    (
        "together",
        &["e0a0a9aa946c37e0692bbea4f43d64c7d9d8eb80389d26a6277296d5eaf6c1a9"],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PythonSchemaFileKind {
    BundledCopy,
    IgnoredG4f,
    Unrepresented,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PythonSchemaFileReview {
    pub name: String,
    pub sha256: String,
    pub kind: PythonSchemaFileKind,
}

/// A value-free, read-only source inventory, not an active Rust schema store.
/// The directory is captured privately for validation before publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PythonSchemaDirectoryReview {
    pub directory_present: bool,
    pub source_sha256: String,
    pub files: Vec<PythonSchemaFileReview>,
    #[serde(skip)]
    source: PathBuf,
}

#[derive(Debug, Error)]
pub enum PythonSchemaAuditError {
    #[error(
        "Python APIModelSchemas must be an absolute, non-symlink directory or absent under a real directory"
    )]
    UnsafeDirectory,
    #[error("Python APIModelSchemas contains an unsafe or unreadable JSON entry")]
    UnsafeEntry,
    #[error("Python APIModelSchemas exceeds the bounded inspection limits")]
    TooLarge,
    #[error("Python APIModelSchemas changed after source review")]
    SourceChanged,
    #[error("{0} Python model schema(s) are not represented in Rust; no files written")]
    Unrepresented(usize),
    #[error("Python APIModelSchemas read failed: {0}")]
    Io(#[from] io::Error),
}

impl PythonSchemaDirectoryReview {
    /// Pass the exact source `/Settings/APIModelSchemas` path, including when
    /// it does not exist. Never creates, repairs or modifies that directory.
    pub fn inspect(path: &Path) -> Result<Self, PythonSchemaAuditError> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(PythonSchemaAuditError::UnsafeDirectory);
        }
        let parent = path
            .parent()
            .ok_or(PythonSchemaAuditError::UnsafeDirectory)?;
        let parent_meta =
            fs::symlink_metadata(parent).map_err(|_| PythonSchemaAuditError::UnsafeDirectory)?;
        if !parent_meta.is_dir() || parent_meta.file_type().is_symlink() {
            return Err(PythonSchemaAuditError::UnsafeDirectory);
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(PythonSchemaAuditError::Io(error)),
        };
        if metadata
            .as_ref()
            .is_some_and(|meta| !meta.is_dir() || meta.file_type().is_symlink())
        {
            return Err(PythonSchemaAuditError::UnsafeDirectory);
        }
        let mut files = Vec::new();
        let mut total = 0_usize;
        if metadata.is_some() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| PythonSchemaAuditError::UnsafeEntry)?;
                if !name.to_ascii_lowercase().ends_with(".json") {
                    continue;
                }
                if files.len() >= MAX_SCHEMAS {
                    return Err(PythonSchemaAuditError::TooLarge);
                }
                let location = entry.path();
                let before = fs::symlink_metadata(&location)?;
                if !before.is_file() || before.file_type().is_symlink() {
                    return Err(PythonSchemaAuditError::UnsafeEntry);
                }
                let length =
                    usize::try_from(before.len()).map_err(|_| PythonSchemaAuditError::TooLarge)?;
                total = total
                    .checked_add(length)
                    .ok_or(PythonSchemaAuditError::TooLarge)?;
                if length > MAX_SCHEMA_BYTES || total > MAX_TOTAL_BYTES {
                    return Err(PythonSchemaAuditError::TooLarge);
                }
                let bytes = fs::read(&location)?;
                let after = fs::symlink_metadata(&location)?;
                if !after.is_file()
                    || after.file_type().is_symlink()
                    || bytes.len() != length
                    || after.len() != before.len()
                {
                    return Err(PythonSchemaAuditError::SourceChanged);
                }
                let kind = classify_schema(&bytes);
                let id = if kind == PythonSchemaFileKind::BundledCopy {
                    let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
                    parse_unique_json(body)
                        .ok()
                        .and_then(|json| json.get("id").and_then(Value::as_str).map(str::to_owned))
                } else {
                    None
                };
                files.push((
                    PythonSchemaFileReview {
                        name,
                        sha256: sha256_hex(&bytes),
                        kind,
                    },
                    id,
                ));
            }
        }
        files.sort_by(|a, b| a.0.name.cmp(&b.0.name));
        let mut factory_ids = BTreeSet::new();
        for (file, id) in &mut files {
            if file.kind == PythonSchemaFileKind::BundledCopy
                && id.as_ref().is_none_or(|id| !factory_ids.insert(id.clone()))
            {
                file.kind = PythonSchemaFileKind::Unrepresented;
            }
        }
        let files: Vec<_> = files.into_iter().map(|(review, _)| review).collect();
        let encoded = serde_json::to_vec(&(metadata.is_some(), &files))
            .map_err(|_| PythonSchemaAuditError::UnsafeEntry)?;
        Ok(Self {
            directory_present: metadata.is_some(),
            source_sha256: sha256_hex(&encoded),
            files,
            source: path.to_path_buf(),
        })
    }

    pub fn unrepresented_names(&self) -> Vec<&str> {
        self.files
            .iter()
            .filter(|entry| entry.kind == PythonSchemaFileKind::Unrepresented)
            .map(|entry| entry.name.as_str())
            .collect()
    }

    /// Fail closed on any file that is not demonstrably a historical bundled
    /// copy or the `g4f` dialect ignored by the original Python loader.
    pub fn require_representable(&self) -> Result<(), PythonSchemaAuditError> {
        let count = self.unrepresented_names().len();
        if count == 0 {
            Ok(())
        } else {
            Err(PythonSchemaAuditError::Unrepresented(count))
        }
    }

    pub fn verify_unchanged(&self) -> Result<(), PythonSchemaAuditError> {
        let fresh = Self::inspect(&self.source)?;
        if fresh == *self {
            Ok(())
        } else {
            Err(PythonSchemaAuditError::SourceChanged)
        }
    }
}

fn classify_schema(bytes: &[u8]) -> PythonSchemaFileKind {
    let content = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let Ok(value) = parse_unique_json(content) else {
        return PythonSchemaFileKind::Unrepresented;
    };
    let Some(schema) = value.as_object() else {
        return PythonSchemaFileKind::Unrepresented;
    };
    if schema.get("dialect").and_then(Value::as_str) == Some("g4f") {
        return PythonSchemaFileKind::IgnoredG4f;
    }
    if schema.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return PythonSchemaFileKind::Unrepresented;
    }
    let Some(id) = schema.get("id").and_then(Value::as_str) else {
        return PythonSchemaFileKind::Unrepresented;
    };
    let Some((_, fingerprints)) = BUNDLED_FINGERPRINTS.iter().find(|(name, _)| *name == id) else {
        return PythonSchemaFileKind::Unrepresented;
    };
    // serde_json's BTreeMap-backed Value serializes in lexicographic key
    // order, in UTF-8, without whitespace, as Python's canonical hash does.
    // A matching cryptographic fingerprint is a strictly vetted factory copy;
    // all uncertain numeric/text normalization cases remain unrepresented.
    let Ok(canonical) = serde_json::to_vec(&value) else {
        return PythonSchemaFileKind::Unrepresented;
    };
    let hash = sha256_hex(&canonical);
    if fingerprints.contains(&hash.as_str()) {
        PythonSchemaFileKind::BundledCopy
    } else {
        PythonSchemaFileKind::Unrepresented
    }
}
