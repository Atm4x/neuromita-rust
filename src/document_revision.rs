//! Trusted local orchestration for immutable Documents revisions and explicit settings activation.
//! Text files own their content, the index is derived, and SettingsApi owns active selection.

use std::path::{Path, PathBuf};

use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    identity::CharacterId,
    knowledge::document_index::{self, DocumentError, DocumentIndex},
    settings::{
        DocumentGenerationSettings, DocumentGenerationSource, SectionSnapshot, SettingsApi,
        SettingsError,
    },
};

#[derive(Debug, Error)]
pub enum DocumentRevisionError {
    #[error(transparent)]
    Documents(#[from] DocumentError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("prepared document revision receipt does not match verified source")]
    InvalidReceipt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRevisionReceipt {
    pub character: CharacterId,
    pub root: PathBuf,
    pub index: PathBuf,
    pub fingerprint: String,
    pub documents: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentRevisionStatus {
    Disabled {
        settings_revision: u64,
    },
    Ready {
        settings_revision: u64,
        revision: DocumentRevisionReceipt,
    },
}

/// A bounded diagnostic classification; never includes document contents or OS paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentRevisionIssue {
    MissingRoot,
    MissingIndex,
    InvalidIndex,
    StaleSource,
    WrongScope,
    UnsafeSource,
    InvalidSource,
    Io,
}

/// Read-only state of an admitted Documents source, including broken selections.
/// A broken selection remains selected until the settings owner changes it explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentRevisionInspection {
    Disabled {
        settings_revision: u64,
    },
    Ready {
        settings_revision: u64,
        revision: DocumentRevisionReceipt,
    },
    Unavailable {
        settings_revision: u64,
        issue: DocumentRevisionIssue,
    },
}

pub struct DocumentRevisionApi;

impl DocumentRevisionApi {
    pub fn prepare(
        root: &Path,
        character: CharacterId,
        revisions_directory: &Path,
    ) -> Result<DocumentRevisionReceipt, DocumentRevisionError> {
        let (path, index) =
            document_index::build_new_revision(root, character, revisions_directory)?;
        let receipt = receipt_for(&path, &index)?;
        Self::verify(&receipt)?;
        Ok(receipt)
    }

    pub fn verify(receipt: &DocumentRevisionReceipt) -> Result<(), DocumentRevisionError> {
        let index =
            document_index::load_verified(&receipt.root, receipt.character, &receipt.index)?;
        if *receipt != receipt_for(&receipt.index, &index)? {
            return Err(DocumentRevisionError::InvalidReceipt);
        }
        Ok(())
    }

    /// Only trusted application callers may change the admitted settings section.
    /// The replacement is compare-and-swap: a stale Settings revision never wins.
    /// Call SettingsApi::flush if durable persistence is required before reporting completion.
    pub fn activate(
        receipt: &DocumentRevisionReceipt,
        expected_settings_revision: u64,
    ) -> Result<SectionSnapshot<DocumentGenerationSettings>, DocumentRevisionError> {
        Self::verify(receipt)?;
        let current = SettingsApi::snapshot::<DocumentGenerationSettings>()?;
        if current.revision != expected_settings_revision {
            return Err(SettingsError::Conflict {
                expected: expected_settings_revision,
                actual: current.revision,
            }
            .into());
        }
        let mut next = current.value;
        next.sources.insert(
            receipt.character,
            DocumentGenerationSource {
                root: receipt.root.clone(),
                index: receipt.index.clone(),
            },
        );
        Ok(SettingsApi::replace(expected_settings_revision, next)?)
    }

    pub fn deactivate(
        character: CharacterId,
        expected_settings_revision: u64,
    ) -> Result<SectionSnapshot<DocumentGenerationSettings>, DocumentRevisionError> {
        let current = SettingsApi::snapshot::<DocumentGenerationSettings>()?;
        if current.revision != expected_settings_revision {
            return Err(SettingsError::Conflict {
                expected: expected_settings_revision,
                actual: current.revision,
            }
            .into());
        }
        let mut next = current.value;
        next.sources.remove(&character);
        Ok(SettingsApi::replace(expected_settings_revision, next)?)
    }

    /// Never changes the active selection, even when its index is missing or invalid.
    /// Unlike `status`, exposes a bounded failure category for trusted operator UI/services.
    pub fn inspect(
        character: CharacterId,
    ) -> Result<DocumentRevisionInspection, DocumentRevisionError> {
        let snapshot = SettingsApi::snapshot::<DocumentGenerationSettings>()?;
        let Some(source) = snapshot.value.sources.get(&character) else {
            return Ok(DocumentRevisionInspection::Disabled {
                settings_revision: snapshot.revision,
            });
        };
        match document_index::load_verified(&source.root, character, &source.index) {
            Ok(index) => Ok(DocumentRevisionInspection::Ready {
                settings_revision: snapshot.revision,
                revision: receipt_for(&source.index, &index)?,
            }),
            Err(error) => Ok(DocumentRevisionInspection::Unavailable {
                settings_revision: snapshot.revision,
                issue: issue_for(&error, &source.root, &source.index),
            }),
        }
    }

    pub fn status(character: CharacterId) -> Result<DocumentRevisionStatus, DocumentRevisionError> {
        let snapshot = SettingsApi::snapshot::<DocumentGenerationSettings>()?;
        let Some(source) = snapshot.value.sources.get(&character) else {
            return Ok(DocumentRevisionStatus::Disabled {
                settings_revision: snapshot.revision,
            });
        };
        let index = document_index::load_verified(&source.root, character, &source.index)?;
        Ok(DocumentRevisionStatus::Ready {
            settings_revision: snapshot.revision,
            revision: receipt_for(&source.index, &index)?,
        })
    }
}

fn receipt_for(
    path: &Path,
    index: &DocumentIndex,
) -> Result<DocumentRevisionReceipt, DocumentRevisionError> {
    let bytes = serde_json::to_vec(index).map_err(|_| DocumentError::InvalidIndex)?;
    let fingerprint = digest(&SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(DocumentRevisionReceipt {
        character: index.character,
        root: PathBuf::from(&index.source_root),
        index: path.to_path_buf(),
        fingerprint,
        documents: index.documents.len(),
    })
}

fn issue_for(error: &DocumentError, root: &Path, index: &Path) -> DocumentRevisionIssue {
    match error {
        DocumentError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(root)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                DocumentRevisionIssue::MissingRoot
            } else if std::fs::symlink_metadata(index)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                DocumentRevisionIssue::MissingIndex
            } else {
                DocumentRevisionIssue::Io
            }
        }
        DocumentError::InvalidIndex => DocumentRevisionIssue::InvalidIndex,
        DocumentError::StaleIndex => DocumentRevisionIssue::StaleSource,
        DocumentError::WrongScope => DocumentRevisionIssue::WrongScope,
        DocumentError::UnsafePath(_) => DocumentRevisionIssue::UnsafeSource,
        DocumentError::Limit | DocumentError::InvalidText => DocumentRevisionIssue::InvalidSource,
        _ => DocumentRevisionIssue::Io,
    }
}
