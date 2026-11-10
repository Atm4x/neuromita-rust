//! Durable, derived progress for one preflighted production installation.
//! An event alone is never evidence that a backend, overlay or asset exists.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::AiService;

const SCHEMA: u8 = 1;
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum InstallOperationError {
    #[error("invalid production installation operation: {0}")]
    Invalid(&'static str),
    #[error("production installation operation journal is corrupted")]
    Corrupted,
    #[error("production installation operation is owned by another process")]
    Busy,
    #[error("production installation operation filesystem error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationStage {
    pub id: String,
    pub repeatable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationModel {
    pub service: AiService,
    pub plugin_id: String,
    pub package_revision: String,
    pub asset_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationFailure {
    SourceChanged,
    Backend,
    Overlay,
    Assets,
    Composition,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationExecution {
    Running,
    Interrupted,
    NeedsReceiptResolution,
    RecordedComplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStageView {
    pub id: String,
    pub state: StageState,
    pub repeatable: bool,
}

/// A journal witness, not a receipt, worker status, or model-ready assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationReadModel {
    pub operation_id: Uuid,
    pub models: Vec<OperationModel>,
    pub stages: Vec<OperationStageView>,
    pub execution: OperationExecution,
    pub last_failure: Option<OperationFailure>,
    pub needs_receipt_recheck: bool,
    pub journal_has_torn_tail: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageState {
    Pending,
    Started,
    Verified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Begin {
        version: u8,
        operation_id: Uuid,
        fingerprint: String,
        stages: Vec<OperationStage>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        models: Vec<OperationModel>,
    },
    Failed {
        kind: OperationFailure,
    },
    Started {
        index: usize,
    },
    Verified {
        index: usize,
    },
    Completed,
}

#[derive(Debug, Clone)]
struct Snapshot {
    operation_id: Uuid,
    stages: Vec<OperationStage>,
    states: Vec<StageState>,
    models: Vec<OperationModel>,
    last_failure: Option<OperationFailure>,
    completed: bool,
}

pub struct InstallOperation {
    file: File,
    _owner: File,
    fingerprint: String,
    snapshot: Snapshot,
}

fn check_directory(path: &Path) -> Result<(), InstallOperationError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(InstallOperationError::Invalid("unsafe operation directory"));
    }
    Ok(())
}

fn optional_directory(path: &Path) -> Result<bool, InstallOperationError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            check_directory(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn ensure_child_directory(path: &Path) -> Result<(), InstallOperationError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => check_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
            check_directory(path)
        }
        Err(error) => Err(error.into()),
    }
}

fn open_owner(path: &Path) -> Result<File, InstallOperationError> {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && (meta.file_type().is_symlink() || !meta.is_file() || meta.len() != 0)
    {
        return Err(InstallOperationError::Corrupted);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != 0 {
        return Err(InstallOperationError::Corrupted);
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(InstallOperationError::Busy),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn valid_models(models: &[OperationModel]) -> bool {
    models.len() <= 5
        && models.iter().all(|model| {
            super::layer::valid_segment(&model.plugin_id)
                && super::layer::valid_segment(&model.package_revision)
                && model
                    .asset_revision
                    .as_deref()
                    .is_none_or(super::layer::valid_segment)
        })
        && models.iter().enumerate().all(|(index, model)| {
            !models[..index]
                .iter()
                .any(|other| other.service == model.service || other.plugin_id == model.plugin_id)
        })
}

fn read_events(
    file: &mut File,
    fingerprint: &str,
    repair_torn_tail: bool,
) -> Result<(Option<Snapshot>, bool), InstallOperationError> {
    if file.metadata()?.len() > MAX_JOURNAL_BYTES {
        return Err(InstallOperationError::Corrupted);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(InstallOperationError::Corrupted);
    }
    let mut snapshot = None;
    let mut offset = 0usize;
    let mut torn = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            torn = true;
            if repair_torn_tail {
                file.set_len(offset as u64)?;
                file.sync_all()?;
            }
            break;
        }
        if line.len() > 8192 {
            return Err(InstallOperationError::Corrupted);
        }
        let event = serde_json::from_slice(line).map_err(|_| InstallOperationError::Corrupted)?;
        apply(&mut snapshot, event, fingerprint)?;
        offset += line.len();
    }
    Ok((snapshot, torn))
}

fn existing_file(path: &Path, for_lock: bool) -> Result<Option<File>, InstallOperationError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(InstallOperationError::Corrupted);
    }
    let file = OpenOptions::new().read(true).write(for_lock).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(InstallOperationError::Corrupted);
        }
    }
    Ok(Some(file))
}

fn apply(
    snapshot: &mut Option<Snapshot>,
    event: Event,
    fingerprint: &str,
) -> Result<(), InstallOperationError> {
    match event {
        Event::Begin {
            version,
            operation_id,
            fingerprint: saved,
            stages,
            models,
        } if version == SCHEMA
            && saved == fingerprint
            && !stages.is_empty()
            && stages.len() <= 64
            && stages.iter().all(|stage| {
                !stage.id.is_empty()
                    && stage.id.len() <= 256
                    && !stage.id.chars().any(char::is_control)
            })
            && valid_models(&models)
            && snapshot.as_ref().is_none_or(|old| old.completed) =>
        {
            let count = stages.len();
            *snapshot = Some(Snapshot {
                operation_id,
                stages,
                states: vec![StageState::Pending; count],
                models,
                last_failure: None,
                completed: false,
            });
        }
        Event::Failed { kind } => {
            let current = snapshot.as_mut().ok_or(InstallOperationError::Corrupted)?;
            if current.completed {
                return Err(InstallOperationError::Corrupted);
            }
            current.last_failure = Some(kind);
        }
        Event::Started { index } => {
            let current = snapshot.as_mut().ok_or(InstallOperationError::Corrupted)?;
            if current.completed
                || current.states.get(index) != Some(&StageState::Pending)
                || current.states[..index]
                    .iter()
                    .any(|state| *state != StageState::Verified)
            {
                return Err(InstallOperationError::Corrupted);
            }
            current.states[index] = StageState::Started;
            current.last_failure = None;
        }
        Event::Verified { index } => {
            let current = snapshot.as_mut().ok_or(InstallOperationError::Corrupted)?;
            if current.completed || current.states.get(index) != Some(&StageState::Started) {
                return Err(InstallOperationError::Corrupted);
            }
            current.states[index] = StageState::Verified;
            current.last_failure = None;
        }
        Event::Completed => {
            let current = snapshot.as_mut().ok_or(InstallOperationError::Corrupted)?;
            if current.completed
                || current
                    .states
                    .iter()
                    .any(|state| *state != StageState::Verified)
            {
                return Err(InstallOperationError::Corrupted);
            }
            current.completed = true;
            current.last_failure = None;
        }
        _ => return Err(InstallOperationError::Corrupted),
    }
    Ok(())
}

impl InstallOperation {
    /// Reopen an unfinished operation with exactly the same pinned preflight fingerprint.
    /// Published stages must be re-verified by their authoritative installers.
    pub fn open(
        ai_data: &Path,
        fingerprint: &str,
        stages: Vec<OperationStage>,
    ) -> Result<Self, InstallOperationError> {
        Self::open_described(ai_data, fingerprint, stages, Vec::new())
    }

    /// Store only public model identity; settings, credentials, and full plan stay private.
    pub fn open_described(
        ai_data: &Path,
        fingerprint: &str,
        stages: Vec<OperationStage>,
        models: Vec<OperationModel>,
    ) -> Result<Self, InstallOperationError> {
        if !valid_models(&models)
            || !ai_data.is_absolute()
            || fingerprint.len() != 64
            || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            || stages.is_empty()
            || stages.len() > 64
            || stages.iter().any(|stage| {
                stage.id.is_empty()
                    || stage.id.len() > 256
                    || stage.id.chars().any(char::is_control)
            })
        {
            return Err(InstallOperationError::Invalid(
                "invalid fingerprint or stages",
            ));
        }
        check_directory(ai_data)?;
        let operations = ai_data.join("operations");
        ensure_child_directory(&operations)?;
        let root = operations.join("production");
        ensure_child_directory(&root)?;
        let _owner = open_owner(&root.join(format!("{fingerprint}.lock")))?;
        let log_path: PathBuf = root.join(format!("{fingerprint}.jsonl"));
        if let Ok(meta) = std::fs::symlink_metadata(&log_path)
            && (!meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_JOURNAL_BYTES)
        {
            return Err(InstallOperationError::Corrupted);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&log_path)?;
        #[cfg(unix)]
        if file.metadata()?.len() == 0 {
            // Make a newly created journal directory entry survive a power loss.
            File::open(&root)?.sync_all()?;
        }
        if file.metadata()?.len() > MAX_JOURNAL_BYTES {
            return Err(InstallOperationError::Corrupted);
        }
        let (snapshot, _) = read_events(&mut file, fingerprint, true)?;
        let next = if let Some(old) = &snapshot {
            if !old.completed {
                if old.stages != stages
                    || (!old.models.is_empty() && old.models != models)
                    || (models.is_empty() && !old.models.is_empty())
                {
                    return Err(InstallOperationError::Invalid(
                        "preflight stages or models changed",
                    ));
                }
                false
            } else {
                true
            }
        } else {
            true
        };
        let mut operation = Self {
            file,
            _owner,
            fingerprint: fingerprint.to_owned(),
            snapshot: snapshot.unwrap_or_else(|| Snapshot {
                operation_id: Uuid::nil(),
                states: Vec::new(),
                stages: Vec::new(),
                models: Vec::new(),
                last_failure: None,
                completed: true,
            }),
        };
        if next {
            operation.append(Event::Begin {
                version: SCHEMA,
                operation_id: Uuid::new_v4(),
                fingerprint: fingerprint.into(),
                stages,
                models,
            })?;
        }
        Ok(operation)
    }

    /// Read an existing operation without creating files, locking out its writer,
    /// repairing torn bytes, or treating journal verification as model readiness.
    pub fn inspect(
        ai_data: &Path,
        fingerprint: &str,
    ) -> Result<Option<OperationReadModel>, InstallOperationError> {
        if !ai_data.is_absolute()
            || fingerprint.len() != 64
            || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(InstallOperationError::Invalid("invalid operation lookup"));
        }
        check_directory(ai_data)?;
        let operations = ai_data.join("operations");
        if !optional_directory(&operations)? {
            return Ok(None);
        }
        let root = operations.join("production");
        if !optional_directory(&root)? {
            return Ok(None);
        }
        let Some(mut journal) = existing_file(&root.join(format!("{fingerprint}.jsonl")), false)?
        else {
            return Ok(None);
        };
        let Some(owner) = existing_file(&root.join(format!("{fingerprint}.lock")), true)? else {
            return Err(InstallOperationError::Corrupted);
        };
        if owner.metadata()?.len() != 0 {
            return Err(InstallOperationError::Corrupted);
        }
        let running = match owner.try_lock() {
            Ok(()) => false,
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        };
        let (snapshot, torn) = read_events(&mut journal, fingerprint, false)?;
        if !running {
            owner.unlock()?;
        }
        Ok(snapshot.map(|snapshot| {
            let execution = if snapshot.completed {
                OperationExecution::RecordedComplete
            } else if running {
                OperationExecution::Running
            } else if snapshot
                .states
                .iter()
                .zip(&snapshot.stages)
                .any(|(state, stage)| *state == StageState::Started && !stage.repeatable)
            {
                OperationExecution::NeedsReceiptResolution
            } else {
                OperationExecution::Interrupted
            };
            OperationReadModel {
                operation_id: snapshot.operation_id,
                models: snapshot.models,
                stages: snapshot
                    .stages
                    .into_iter()
                    .zip(snapshot.states)
                    .map(|(stage, state)| OperationStageView {
                        id: stage.id,
                        repeatable: stage.repeatable,
                        state,
                    })
                    .collect(),
                execution,
                last_failure: snapshot.last_failure,
                needs_receipt_recheck: true,
                journal_has_torn_tail: torn,
            }
        }))
    }

    /// Enumerate only bounded Core-owned journals without exposing private fingerprints.
    /// If a managed record is corrupt, refuse the entire projection.
    pub fn inspect_all(ai_data: &Path) -> Result<Vec<OperationReadModel>, InstallOperationError> {
        if !ai_data.is_absolute() {
            return Err(InstallOperationError::Invalid("invalid operation root"));
        }
        check_directory(ai_data)?;
        let operations = ai_data.join("operations");
        if !optional_directory(&operations)? {
            return Ok(Vec::new());
        }
        let root = operations.join("production");
        if !optional_directory(&root)? {
            return Ok(Vec::new());
        }
        let mut snapshots = Vec::new();
        for (index, entry) in std::fs::read_dir(root)?.enumerate() {
            if index >= 512 {
                return Err(InstallOperationError::Invalid(
                    "operation directory inventory limit reached",
                ));
            }
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(fingerprint) = name.strip_suffix(".jsonl") else {
                continue;
            };
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(InstallOperationError::Corrupted);
            }
            if snapshots.len() >= 128 {
                return Err(InstallOperationError::Invalid(
                    "operation journal inventory limit reached",
                ));
            }
            snapshots.push(
                Self::inspect(ai_data, fingerprint)?.ok_or(InstallOperationError::Corrupted)?,
            );
        }
        snapshots.sort_unstable_by_key(|view| view.operation_id);
        Ok(snapshots)
    }

    pub fn record_failure(&mut self, kind: OperationFailure) -> Result<(), InstallOperationError> {
        self.append(Event::Failed { kind })
    }

    fn append(&mut self, event: Event) -> Result<(), InstallOperationError> {
        if self.file.metadata()?.len() > MAX_JOURNAL_BYTES - 8192 {
            return Err(InstallOperationError::Invalid(
                "operation journal limit reached",
            ));
        }
        let mut next = Some(self.snapshot.clone());
        apply(&mut next, event.clone(), &self.fingerprint)?;
        let mut payload =
            serde_json::to_vec(&event).map_err(|_| InstallOperationError::Corrupted)?;
        payload.push(b'\n');
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&payload)?;
        self.file.sync_data()?;
        self.snapshot = next.ok_or(InstallOperationError::Corrupted)?;
        Ok(())
    }

    pub fn operation_id(&self) -> Uuid {
        self.snapshot.operation_id
    }

    pub fn stage_state(&self, index: usize) -> Result<StageState, InstallOperationError> {
        self.snapshot
            .states
            .get(index)
            .copied()
            .ok_or(InstallOperationError::Invalid("stage index out of bounds"))
    }

    pub fn start(&mut self, index: usize) -> Result<(), InstallOperationError> {
        match self.stage_state(index)? {
            StageState::Pending => self.append(Event::Started { index }),
            StageState::Started if self.snapshot.stages[index].repeatable => Ok(()),
            StageState::Verified => Ok(()),
            StageState::Started => Err(InstallOperationError::Invalid(
                "interrupted nonrepeatable stage requires manual recovery",
            )),
        }
    }

    /// Call only after independently validating the installed immutable receipt.
    pub fn verified(&mut self, index: usize) -> Result<(), InstallOperationError> {
        match self.stage_state(index)? {
            StageState::Started => self.append(Event::Verified { index }),
            StageState::Verified => Ok(()),
            StageState::Pending => Err(InstallOperationError::Invalid("stage was never started")),
        }
    }

    pub fn finish(&mut self) -> Result<(), InstallOperationError> {
        self.append(Event::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, String, Vec<OperationStage>) {
        let root = std::env::temp_dir().join(format!("nm-operation-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        (
            root,
            "d".repeat(64),
            vec![
                OperationStage {
                    id: "tts/backend".into(),
                    repeatable: true,
                },
                OperationStage {
                    id: "tts/generated".into(),
                    repeatable: false,
                },
            ],
        )
    }

    #[test]
    fn interrupted_repeatable_stage_reopens_but_unverified_stage_cannot_complete() {
        let (root, digest, stages) = fixture();
        let id = {
            let mut operation = InstallOperation::open(&root, &digest, stages.clone()).unwrap();
            let id = operation.operation_id();
            operation.start(0).unwrap();
            assert!(operation.verified(1).is_err());
            id
        };
        let mut recovered = InstallOperation::open(&root, &digest, stages.clone()).unwrap();
        assert_eq!(recovered.operation_id(), id);
        assert_eq!(recovered.stage_state(0).unwrap(), StageState::Started);
        recovered.start(0).unwrap();
        recovered.verified(0).unwrap();
        recovered.start(1).unwrap();
        drop(recovered);
        let mut blocked = InstallOperation::open(&root, &digest, stages.clone()).unwrap();
        assert!(blocked.start(1).is_err());
        assert!(blocked.finish().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matching_completed_operation_creates_new_id_and_does_not_reuse_old_verifications() {
        let (root, digest, mut stages) = fixture();
        stages.truncate(1);
        let first = {
            let mut operation = InstallOperation::open(&root, &digest, stages.clone()).unwrap();
            operation.start(0).unwrap();
            operation.verified(0).unwrap();
            operation.finish().unwrap();
            operation.operation_id()
        };
        let mut next = InstallOperation::open(&root, &digest, stages).unwrap();
        assert_ne!(next.operation_id(), first);
        assert_eq!(next.stage_state(0).unwrap(), StageState::Pending);
        next.start(0).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_concurrent_writer_and_corrupted_journal_without_replacing_it() {
        let (root, digest, stages) = fixture();
        let operation = InstallOperation::open(&root, &digest, stages.clone()).unwrap();
        assert!(matches!(
            InstallOperation::open(&root, &digest, stages.clone()),
            Err(InstallOperationError::Busy)
        ));
        drop(operation);
        let path = root
            .join("operations/production")
            .join(format!("{digest}.jsonl"));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{not-json}\n")
            .unwrap();
        assert!(matches!(
            InstallOperation::open(&root, &digest, stages),
            Err(InstallOperationError::Corrupted)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncated_trailing_event_is_ignored_after_restart() {
        let (root, digest, stages) = fixture();
        {
            let mut operation = InstallOperation::open(&root, &digest, stages.clone()).unwrap();
            operation.start(0).unwrap();
        }
        let path = root
            .join("operations/production")
            .join(format!("{digest}.jsonl"));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"event\":\"verified\"")
            .unwrap();
        let operation = InstallOperation::open(&root, &digest, stages).unwrap();
        assert_eq!(operation.stage_state(0).unwrap(), StageState::Started);
        std::fs::remove_dir_all(root).unwrap();
    }
}
