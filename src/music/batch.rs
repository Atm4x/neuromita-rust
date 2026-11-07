//! Deterministic bounded build-missing WAV cache indexing.
use super::{
    BeatResultOrigin,
    runtime::{BeatRuntime, BeatRuntimeError, MusicJob},
    wav::WavError,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

const MAX_DEPTH: usize = 16;
const MAX_FILES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchSummary {
    pub scanned: usize,
    pub cache_hits: usize,
    pub generated: usize,
    pub failed: usize,
}

#[derive(Debug, Error)]
pub enum BatchError {
    #[error("batch cancelled")]
    Cancelled,
    #[error("batch directory traversal failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("batch exceeds file/depth limit")]
    Limit,
    #[error("batch root must be an actual directory")]
    InvalidRoot,
}

fn walk(
    root: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
    cancelled: &impl Fn() -> bool,
) -> Result<(), BatchError> {
    if cancelled() {
        return Err(BatchError::Cancelled);
    }
    if depth > MAX_DEPTH {
        return Err(BatchError::Limit);
    }
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        if cancelled() {
            return Err(BatchError::Cancelled);
        }
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            walk(&entry.path(), depth + 1, out, cancelled)?;
        } else if ty.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        {
            if out.len() >= MAX_FILES {
                return Err(BatchError::Limit);
            }
            out.push(entry.path());
        }
    }
    Ok(())
}

impl BeatRuntime {
    /// The current native implementation can index WAV files only.
    /// Cache hits are reused; this is *not* an unconditional rebuild.
    /// The progress callback receives a snapshot after each processed file.
    pub fn index_wav_directory(
        &self,
        root: &Path,
        template: &MusicJob<'_>,
        cancelled: &impl Fn() -> bool,
        mut progress: impl FnMut(BatchSummary),
    ) -> Result<BatchSummary, BatchError> {
        let metadata = fs::symlink_metadata(root)?;
        if !metadata.file_type().is_dir() {
            return Err(BatchError::InvalidRoot);
        }
        let mut paths = Vec::new();
        walk(root, 0, &mut paths, cancelled)?;
        paths.sort();
        let mut summary = BatchSummary::default();
        for path in paths {
            if cancelled() {
                return Err(BatchError::Cancelled);
            }
            summary.scanned += 1;
            let result = self.read_and_analyze_path(&path, template, cancelled);
            match result {
                Ok(BeatResultOrigin::Cache) => summary.cache_hits += 1,
                Ok(BeatResultOrigin::Analysis) => summary.generated += 1,
                Err(BeatRuntimeError::Audio(WavError::Cancelled)) => {
                    return Err(BatchError::Cancelled);
                }
                Err(_) => summary.failed += 1,
            }
            progress(summary);
        }
        Ok(summary)
    }

    fn read_and_analyze_path(
        &self,
        path: &Path,
        template: &MusicJob<'_>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<BeatResultOrigin, BeatRuntimeError> {
        if cancelled() {
            return Err(WavError::Cancelled.into());
        }
        let bytes = Self::read_source(path)?;
        let job = MusicJob {
            request_id: template.request_id,
            track_name: template.track_name,
            audio_path: template.audio_path,
            stream: false,
            min_confidence: 0.0,
            backend: template.backend,
            enabled: template.enabled,
        };
        self.analyze(&bytes, &job, cancelled)
            .map(|result| result.origin)
    }
}
