//! Offline, recoverable activation of a reviewed Python preset snapshot.
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use thiserror::Error;

use super::PythonPresetStage;
use super::python_presets::{parse_credentials, parse_unique_json};
use crate::generation::StoredPresets;

const JOURNAL: &str = ".preset-activation-journal";
const NAMES: [&str; 2] = ["presets.json", "preset-credentials.json"];
const SOURCES: [&str; 2] = [
    "existing-source-presets.json",
    "existing-source-preset-credentials.json",
];
const MAX_BYTES: [usize; 2] = [8 * 1024 * 1024, 1_048_576];

#[derive(Debug, Error)]
pub enum PresetActivationError {
    #[error("unsafe or missing preset activation directory")]
    UnsafePath,
    #[error("preset activation requires an explicit recovery before starting the app")]
    RecoveryRequired,
    #[error("preset activation snapshot is invalid or no longer matches the stopped profile")]
    SnapshotConflict,
    #[error(
        "activation already committed but cleanup was interrupted; verify installed files before retry"
    )]
    CommittedNeedsCleanup,
    #[error("preset activation I/O: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetRecovery {
    NoJournal,
    RolledBack,
    Committed,
    DiscardedUnprepared,
}

pub struct PresetActivation;

impl PresetActivation {
    /// Call at the application composition root BEFORE opening either preset file.
    pub fn ensure_ready(active: &Path) -> Result<(), PresetActivationError> {
        match fs::symlink_metadata(active.join(JOURNAL)) {
            Ok(_) => Err(PresetActivationError::RecoveryRequired),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PresetActivationError::Io(error)),
        }
    }

    /// Must only be called by an operator with both applications stopped.
    /// Never modifies the source review package, and never runs during startup.
    pub fn apply(review: &Path, active: &Path) -> Result<(), PresetActivationError> {
        Self::apply_inner(review, active, None)
    }

    fn apply_inner(
        review: &Path,
        active: &Path,
        fail_after_install: Option<usize>,
    ) -> Result<(), PresetActivationError> {
        check_dir(review)?;
        check_dir(active)?;
        Self::ensure_ready(active)?;
        if fs::canonicalize(review)? == fs::canonicalize(active)? {
            return Err(PresetActivationError::UnsafePath);
        }
        let mut new = Vec::with_capacity(2);
        let mut old = Vec::with_capacity(2);
        for i in 0..2 {
            let incoming = read_file(&review.join(NAMES[i]), MAX_BYTES[i])?
                .ok_or(PresetActivationError::SnapshotConflict)?;
            let previous = read_file(&active.join(NAMES[i]), MAX_BYTES[i])?;
            let expected = read_file(&review.join(SOURCES[i]), MAX_BYTES[i])?;
            if previous != expected {
                return Err(PresetActivationError::SnapshotConflict);
            }
            new.push(incoming);
            old.push(previous);
        }
        // Recreate the reviewed output from the source backups. A manually
        // edited or stale review package may not silently replace a profile.
        let python_presets =
            read_file(&review.join("python-source-api-presets.json"), MAX_BYTES[0])?
                .ok_or(PresetActivationError::SnapshotConflict)?;
        let python_templates = read_file(
            &review.join("python-source-api-templates.json"),
            MAX_BYTES[0],
        )?
        .ok_or(PresetActivationError::SnapshotConflict)?;
        let source_plan = PythonPresetStage::prepare(&python_presets, &python_templates)
            .map_err(|_| PresetActivationError::SnapshotConflict)?;
        let expected_plan = if old.iter().any(Option::is_some) {
            source_plan
                .merge_with_existing(old[0].as_deref(), old[1].as_deref())
                .map_err(|_| PresetActivationError::SnapshotConflict)?
        } else {
            source_plan
        };
        for (i, bytes) in new.iter().enumerate() {
            if expected_plan
                .file(NAMES[i])
                .map_err(|_| PresetActivationError::SnapshotConflict)?
                != bytes
            {
                return Err(PresetActivationError::SnapshotConflict);
            }
        }
        validate_pair(&new[0], &new[1])?;
        if let Some(previous) = &old[0] {
            let credentials = old[1].as_deref().unwrap_or(b"{}");
            validate_pair(previous, credentials)?;
        } else if old[1].is_some() {
            parse_credentials(old[1].as_deref())
                .map_err(|_| PresetActivationError::SnapshotConflict)?;
        }
        let journal = active.join(JOURNAL);
        create_private_dir(&journal)?;
        let prepared = (|| -> Result<(), PresetActivationError> {
            for (i, bytes) in new.iter().enumerate() {
                write_private(&journal.join(format!("new-{i}")), bytes)?;
                if let Some(old_bytes) = &old[i] {
                    write_private(&journal.join(format!("old-{i}")), old_bytes)?;
                    write_private(&journal.join(format!("present-{i}")), b"")?;
                }
            }
            sync_dir(&journal)?;
            write_private(&journal.join("prepared"), b"")?;
            sync_dir(&journal)?;
            sync_dir(active)?;
            Ok(())
        })();
        if let Err(error) = prepared {
            // No active file was touched before the durable `prepared` marker.
            return Err(match Self::recover(active) {
                Ok(_) => error,
                Err(_) => PresetActivationError::RecoveryRequired,
            });
        }
        crash_test_checkpoint("prepared");
        let result = (|| -> Result<(), PresetActivationError> {
            // Refuse stale snapshots before the first mutation. This is not a
            // replacement for ensuring the application has been shut down.
            for i in 0..2 {
                if read_file(&active.join(NAMES[i]), MAX_BYTES[i])? != old[i] {
                    return Err(PresetActivationError::SnapshotConflict);
                }
            }
            for i in 0..2 {
                install_bytes(&journal, active, NAMES[i], &new[i])?;
                crash_test_checkpoint(if i == 0 { "installed-0" } else { "installed-1" });
                if fail_after_install == Some(i) {
                    return Err(PresetActivationError::SnapshotConflict);
                }
            }
            sync_dir(active)?;
            write_private(&journal.join("committed"), b"")?;
            sync_dir(&journal)?;
            crash_test_checkpoint("committed");
            Ok(())
        })();
        if let Err(error) = result {
            return Err(match Self::recover(active) {
                Ok(PresetRecovery::Committed) => PresetActivationError::CommittedNeedsCleanup,
                Ok(_) => error,
                Err(_) => PresetActivationError::RecoveryRequired,
            });
        }
        // The transaction is committed and durable; cleanup failure is not a rollback.
        finish_journal(active, &journal).map_err(|_| PresetActivationError::CommittedNeedsCleanup)
    }

    /// Explicit stopped-process recovery. A prepared but uncommitted transaction
    /// is rolled back; a committed transaction is left installed.
    pub fn recover(active: &Path) -> Result<PresetRecovery, PresetActivationError> {
        check_dir(active)?;
        let journal = active.join(JOURNAL);
        match fs::symlink_metadata(&journal) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PresetRecovery::NoJournal);
            }
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => return Err(PresetActivationError::UnsafePath),
        }
        let committed = read_file(&journal.join("committed"), 0)?.is_some();
        let prepared = read_file(&journal.join("prepared"), 0)?.is_some();
        if committed {
            if !prepared {
                return Err(PresetActivationError::RecoveryRequired);
            }
            for (i, name) in NAMES.iter().enumerate() {
                let new = read_file(&journal.join(format!("new-{i}")), MAX_BYTES[i])?
                    .ok_or(PresetActivationError::RecoveryRequired)?;
                if read_file(&active.join(name), MAX_BYTES[i])?.as_deref() != Some(new.as_slice()) {
                    return Err(PresetActivationError::RecoveryRequired);
                }
            }
        } else if prepared {
            let mut old = Vec::with_capacity(2);
            for (i, limit) in MAX_BYTES.iter().enumerate() {
                let snapshot = read_file(&journal.join(format!("old-{i}")), *limit)?;
                let present = read_file(&journal.join(format!("present-{i}")), 0)?.is_some();
                if snapshot.is_some() != present {
                    return Err(PresetActivationError::RecoveryRequired);
                }
                let next = read_file(&journal.join(format!("new-{i}")), *limit)?
                    .ok_or(PresetActivationError::RecoveryRequired)?;
                let current = read_file(&active.join(NAMES[i]), *limit)?;
                if let Some(current) = current.as_deref()
                    && Some(current) != snapshot.as_deref()
                    && current != next
                {
                    // An external writer changed the file after the crash.
                    // Refuse recovery rather than overwriting unrelated data.
                    return Err(PresetActivationError::SnapshotConflict);
                }
                old.push(snapshot);
            }
            for i in 0..2 {
                match &old[i] {
                    Some(bytes) => install_bytes(&journal, active, NAMES[i], bytes)?,
                    None => remove_target(&journal, active, NAMES[i])?,
                }
                if i == 0 {
                    crash_test_checkpoint("recovered-0");
                }
            }
            sync_dir(active)?;
        }
        let kind = if committed {
            PresetRecovery::Committed
        } else if prepared {
            PresetRecovery::RolledBack
        } else {
            PresetRecovery::DiscardedUnprepared
        };
        finish_journal(active, &journal)?;
        Ok(kind)
    }
}

fn validate_pair(catalog: &[u8], credentials: &[u8]) -> Result<(), PresetActivationError> {
    let parsed: StoredPresets = serde_json::from_value(
        parse_unique_json(catalog).map_err(|_| PresetActivationError::SnapshotConflict)?,
    )
    .map_err(|_| PresetActivationError::SnapshotConflict)?;
    parsed
        .validate_for_activation()
        .map_err(|_| PresetActivationError::SnapshotConflict)?;
    let secrets = parse_credentials(Some(credentials))
        .map_err(|_| PresetActivationError::SnapshotConflict)?;
    if parsed
        .local_credentials()
        .iter()
        .any(|name| !secrets.contains_key(*name))
    {
        return Err(PresetActivationError::SnapshotConflict);
    }
    Ok(())
}

fn check_dir(path: &Path) -> Result<(), PresetActivationError> {
    if !path.is_absolute() {
        return Err(PresetActivationError::UnsafePath);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| PresetActivationError::UnsafePath)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PresetActivationError::UnsafePath);
    }
    Ok(())
}

fn read_file(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, PresetActivationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(PresetActivationError::UnsafePath);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        _ => {}
    }
    if fs::metadata(path)?.len() > limit as u64 {
        return Err(PresetActivationError::SnapshotConflict);
    }
    let bytes = fs::read(path)?;
    if bytes.len() > limit {
        return Err(PresetActivationError::SnapshotConflict);
    }
    Ok(Some(bytes))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

fn install_bytes(
    journal: &Path,
    active: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), PresetActivationError> {
    let temp = journal.join(format!("install-{}", uuid::Uuid::new_v4().simple()));
    write_private(&temp, bytes)?;
    remove_target(journal, active, name)?;
    fs::rename(&temp, active.join(name))?;
    Ok(())
}

fn remove_target(journal: &Path, active: &Path, name: &str) -> Result<(), PresetActivationError> {
    let target = active.join(name);
    if read_file(&target, MAX_BYTES[if name == NAMES[0] { 0 } else { 1 }])?.is_some() {
        fs::rename(
            target,
            journal.join(format!("discard-{}", uuid::Uuid::new_v4().simple())),
        )?;
    }
    Ok(())
}

fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn finish_journal(active: &Path, journal: &Path) -> Result<(), PresetActivationError> {
    let finished = active.join(format!(
        ".preset-activation-finished-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::rename(journal, &finished)?;
    sync_dir(active)?;
    fs::remove_dir_all(&finished)?;
    sync_dir(active)?;
    Ok(())
}

// Crash points are compiled into unit tests only. Production builds have no
// environment-controlled termination path.
#[cfg(test)]
fn crash_test_checkpoint(point: &str) {
    if std::env::var("NEUROMITA_PRESET_TEST_ABORT_AT").as_deref() == Ok(point) {
        std::process::abort();
    }
}

#[cfg(not(test))]
#[inline(always)]
fn crash_test_checkpoint(_: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct Root(std::path::PathBuf);
    impl Root {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("preset-tx-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn incomplete_prepared_journal_is_discarded_without_touching_targets() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        write_private(&journal.join("new-0"), b"private").unwrap();
        assert!(matches!(
            PresetActivation::ensure_ready(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::DiscardedUnprepared
        );
        assert!(!journal.exists());
    }

    #[test]
    fn interruption_between_two_file_installs_restores_exact_old_bytes() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        let old = [b"old-records".as_slice(), b"old-keys".as_slice()];
        let next = [b"new-records".as_slice(), b"new-keys".as_slice()];
        for (i, name) in NAMES.iter().enumerate() {
            write_private(&root.path().join(name), old[i]).unwrap();
            write_private(&journal.join(format!("old-{i}")), old[i]).unwrap();
            write_private(&journal.join(format!("present-{i}")), b"").unwrap();
            write_private(&journal.join(format!("new-{i}")), next[i]).unwrap();
        }
        write_private(&journal.join("prepared"), b"").unwrap();
        install_bytes(&journal, root.path(), NAMES[0], next[0]).unwrap();
        assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), next[0]);
        assert!(matches!(
            PresetActivation::ensure_ready(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::RolledBack
        );
        for (i, name) in NAMES.iter().enumerate() {
            assert_eq!(fs::read(root.path().join(name)).unwrap(), old[i]);
        }
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::NoJournal
        );
    }

    #[test]
    fn committed_transaction_is_never_rolled_back_and_can_be_cleaned() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for (i, name) in NAMES.iter().enumerate() {
            let bytes = format!("new-{i}");
            write_private(&root.path().join(name), bytes.as_bytes()).unwrap();
            write_private(&journal.join(format!("new-{i}")), bytes.as_bytes()).unwrap();
        }
        write_private(&journal.join("prepared"), b"").unwrap();
        write_private(&journal.join("committed"), b"").unwrap();
        assert_eq!(
            PresetActivation::recover(root.path()).unwrap(),
            PresetRecovery::Committed
        );
        assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), b"new-0");
        assert!(!journal.exists());
    }

    #[test]
    fn failed_first_install_rolls_back_and_retry_can_commit() {
        let root = Root::new();
        let active = root.path().join("UserData");
        fs::create_dir(&active).unwrap();
        let review = root.path().join("review");
        let templates = serde_json::json!({"templates":{"3":{"name":"Google","protocol_id":"google_gemini_default","url":"","default_model":"gemini-2.5-flash"}}});
        let presets = serde_json::json!({"presets":{"18":{
            "id":18,"name":"Offline test","base":3,"protocol_id":"","protocol_overrides":{},
            "url":"","default_model":"gemini-2.5-flash","key":"fixture-token",
            "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},
            "enabled":["max_tokens","thinking_level"]}
        }}});
        let prepared = PythonPresetStage::prepare(
            &serde_json::to_vec(&presets).unwrap(),
            &serde_json::to_vec(&templates).unwrap(),
        )
        .unwrap();
        prepared.stage_new_directory(&review).unwrap();
        assert!(matches!(
            PresetActivation::apply_inner(&review, &active, Some(0)),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert!(!active.join(NAMES[0]).exists());
        assert!(!active.join(NAMES[1]).exists());
        assert!(!active.join(JOURNAL).exists());
        PresetActivation::apply(&review, &active).unwrap();
        assert_eq!(
            fs::read(active.join(NAMES[0])).unwrap(),
            fs::read(review.join(NAMES[0])).unwrap()
        );
        assert_eq!(
            fs::read(active.join(NAMES[1])).unwrap(),
            fs::read(review.join(NAMES[1])).unwrap()
        );
    }

    #[test]
    fn missing_old_backup_is_never_interpreted_as_original_absence() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for i in 0..2 {
            write_private(&journal.join(format!("new-{i}")), b"replacement").unwrap();
        }
        write_private(&journal.join("present-0"), b"").unwrap();
        write_private(&journal.join("prepared"), b"").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert!(journal.exists());
    }

    #[test]
    fn a_committed_but_mismatched_snapshot_requires_operator_repair() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        write_private(&journal.join("committed"), b"").unwrap();
        write_private(&journal.join("new-0"), b"expected").unwrap();
        write_private(&journal.join("new-1"), b"secret").unwrap();
        write_private(&root.path().join(NAMES[0]), b"mismatch").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert!(journal.exists());
    }

    fn fixture_input(id: u32) -> (Vec<u8>, Vec<u8>) {
        let templates = serde_json::json!({"templates":{"3":{
            "name":"Google", "protocol_id":"google_gemini_default",
            "url":"", "default_model":"gemini-2.5-flash"
        }}});
        let presets = serde_json::json!({"presets":{id.to_string():{
            "id":id, "name":format!("Preset {id}"), "base":3,
            "protocol_id":"", "protocol_overrides":{}, "url":"",
            "default_model":"gemini-2.5-flash", "key":format!("private-{id}"),
            "model_settings":{"values":{
                "temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"
            },"enabled":["max_tokens","thinking_level"]}
        }}});
        (
            serde_json::to_vec(&presets).unwrap(),
            serde_json::to_vec(&templates).unwrap(),
        )
    }

    #[cfg(unix)]
    fn crash_fixture(
        root: &Root,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        [Vec<u8>; 2],
        [Vec<u8>; 2],
    ) {
        let active = root.path().join("UserData");
        fs::create_dir(&active).unwrap();
        let (old_presets, old_templates) = fixture_input(7);
        let old_stage = PythonPresetStage::prepare(&old_presets, &old_templates).unwrap();
        let old = [
            old_stage.file(NAMES[0]).unwrap().to_vec(),
            old_stage.file(NAMES[1]).unwrap().to_vec(),
        ];
        for (name, bytes) in NAMES.iter().zip(old.iter()) {
            write_private(&active.join(name), bytes).unwrap();
        }
        let (incoming, templates) = fixture_input(18);
        let next_stage = PythonPresetStage::prepare(&incoming, &templates)
            .unwrap()
            .merge_with_existing(Some(&old[0]), Some(&old[1]))
            .unwrap();
        let next = [
            next_stage.file(NAMES[0]).unwrap().to_vec(),
            next_stage.file(NAMES[1]).unwrap().to_vec(),
        ];
        let review = root.path().join("review с пробелами");
        next_stage.stage_new_directory(&review).unwrap();
        (review, active, old, next)
    }

    #[cfg(unix)]
    #[test]
    fn abort_child_runner() {
        let Ok(marker) = std::env::var("NEUROMITA_PRESET_TEST_ABORT_AT") else {
            return;
        };
        let active = std::path::PathBuf::from(std::env::var_os("NEUROMITA_TEST_ACTIVE").unwrap());
        if marker == "recovered-0" {
            PresetActivation::recover(&active).unwrap();
        } else {
            let review =
                std::path::PathBuf::from(std::env::var_os("NEUROMITA_TEST_REVIEW").unwrap());
            PresetActivation::apply(&review, &active).unwrap();
        }
        panic!("crash injection point {marker} was not reached");
    }

    #[cfg(unix)]
    fn abort_process_at(marker: &str, review: &Path, active: &Path) {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("settings::preset_activation::tests::abort_child_runner")
            .arg("--nocapture")
            .env("NEUROMITA_PRESET_TEST_ABORT_AT", marker)
            .env("NEUROMITA_TEST_REVIEW", review)
            .env("NEUROMITA_TEST_ACTIVE", active)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(6),
            "{marker}: child did not abort at checkpoint: {status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn actual_subprocess_crashes_recover_both_files_at_every_commit_boundary() {
        for marker in ["prepared", "installed-0", "installed-1", "committed"] {
            let root = Root::new();
            let (review, active, old, next) = crash_fixture(&root);
            abort_process_at(marker, &review, &active);
            assert!(matches!(
                PresetActivation::ensure_ready(&active),
                Err(PresetActivationError::RecoveryRequired)
            ));
            let result = PresetActivation::recover(&active).unwrap();
            let expected = if marker == "committed" {
                assert_eq!(result, PresetRecovery::Committed);
                &next
            } else {
                assert_eq!(result, PresetRecovery::RolledBack);
                &old
            };
            for (name, bytes) in NAMES.iter().zip(expected.iter()) {
                assert_eq!(
                    fs::read(active.join(name)).unwrap(),
                    *bytes,
                    "{marker} {name}"
                );
            }
            assert_eq!(
                PresetActivation::recover(&active).unwrap(),
                PresetRecovery::NoJournal
            );
            PresetActivation::ensure_ready(&active).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn recovery_is_idempotent_after_a_second_real_process_crash() {
        let root = Root::new();
        let (review, active, old, _) = crash_fixture(&root);
        abort_process_at("installed-0", &review, &active);
        abort_process_at("recovered-0", &review, &active);
        assert!(matches!(
            PresetActivation::ensure_ready(&active),
            Err(PresetActivationError::RecoveryRequired)
        ));
        assert_eq!(
            PresetActivation::recover(&active).unwrap(),
            PresetRecovery::RolledBack
        );
        for (name, bytes) in NAMES.iter().zip(old.iter()) {
            assert_eq!(fs::read(active.join(name)).unwrap(), *bytes);
        }
        assert_eq!(
            PresetActivation::recover(&active).unwrap(),
            PresetRecovery::NoJournal
        );
    }

    #[test]
    fn recovery_refuses_external_mutation_before_replacing_either_file() {
        let root = Root::new();
        let journal = root.path().join(JOURNAL);
        create_private_dir(&journal).unwrap();
        for (i, name) in NAMES.iter().enumerate() {
            write_private(
                &journal.join(format!("old-{i}")),
                format!("old-{i}").as_bytes(),
            )
            .unwrap();
            write_private(
                &journal.join(format!("new-{i}")),
                format!("new-{i}").as_bytes(),
            )
            .unwrap();
            write_private(&journal.join(format!("present-{i}")), b"").unwrap();
            write_private(&root.path().join(name), format!("new-{i}").as_bytes()).unwrap();
        }
        write_private(&journal.join("prepared"), b"").unwrap();
        fs::write(root.path().join(NAMES[1]), b"outside writer data").unwrap();
        assert!(matches!(
            PresetActivation::recover(root.path()),
            Err(PresetActivationError::SnapshotConflict)
        ));
        assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), b"new-0");
        assert_eq!(
            fs::read(root.path().join(NAMES[1])).unwrap(),
            b"outside writer data"
        );
        assert!(journal.exists());
    }
}
