use std::{fs::OpenOptions, io::Write, path::PathBuf};

use neuromita::{
    ai_runtime::{
        AiService,
        install_operation::{
            InstallOperation, InstallOperationError, OperationExecution, OperationFailure,
            OperationModel, OperationStage, StageState,
        },
    },
    presentation::AiInstallOperationPresenter,
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    digest: String,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nm-install-view-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self {
            root,
            digest: "9".repeat(64),
        }
    }

    fn stages() -> Vec<OperationStage> {
        vec![
            OperationStage {
                id: "0:primary-backend:onnx-cpu".into(),
                repeatable: true,
            },
            OperationStage {
                id: "0:assets:asr_cpu".into(),
                repeatable: false,
            },
        ]
    }

    fn models() -> Vec<OperationModel> {
        vec![OperationModel {
            service: AiService::Asr,
            plugin_id: "asr_cpu".into(),
            package_revision: "v1".into(),
            asset_revision: Some("weights-v1".into()),
        }]
    }

    fn open(&self) -> InstallOperation {
        InstallOperation::open_described(&self.root, &self.digest, Self::stages(), Self::models())
            .unwrap()
    }

    fn read(&self) -> neuromita::ai_runtime::install_operation::OperationReadModel {
        AiInstallOperationPresenter::new(self.root.clone())
            .read(&self.digest)
            .unwrap()
            .unwrap()
    }

    fn journal(&self) -> PathBuf {
        self.root
            .join("operations/production")
            .join(format!("{}.jsonl", self.digest))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn read_absent_is_non_mutating() {
    let fixture = Fixture::new();
    let presenter = AiInstallOperationPresenter::new(fixture.root.clone());
    assert!(presenter.read(&fixture.digest).unwrap().is_none());
    assert!(!fixture.root.join("operations").exists());
}

#[test]
fn running_writer_does_not_block_inspection_or_advertise_readiness() {
    let fixture = Fixture::new();
    let mut owner = fixture.open();
    owner.start(0).unwrap();
    owner.verified(0).unwrap();
    let view = fixture.read();
    assert_eq!(view.execution, OperationExecution::Running);
    assert_eq!(view.operation_id, owner.operation_id());
    assert_eq!(view.models, Fixture::models());
    assert_eq!(view.stages[0].state, StageState::Verified);
    assert_eq!(view.stages[1].state, StageState::Pending);
    assert!(view.needs_receipt_recheck);
    assert_eq!(view.last_failure, None);
    assert!(!view.journal_has_torn_tail);
}

#[test]
fn interruption_and_explicit_sanitized_failure_are_visible() {
    let fixture = Fixture::new();
    let id = {
        let mut owner = fixture.open();
        owner.start(0).unwrap();
        owner.record_failure(OperationFailure::Backend).unwrap();
        owner.operation_id()
    };
    let view = fixture.read();
    assert_eq!(view.operation_id, id);
    assert_eq!(view.execution, OperationExecution::Interrupted);
    assert_eq!(view.last_failure, Some(OperationFailure::Backend));
    assert!(
        !std::fs::read_to_string(fixture.journal())
            .unwrap()
            .contains("password")
    );
    let mut resumed = fixture.open();
    resumed.verified(0).unwrap();
    assert_eq!(fixture.read().last_failure, None);
}

#[test]
fn interrupted_nonrepeatable_stage_requires_manual_receipt_resolution() {
    let fixture = Fixture::new();
    {
        let mut owner = fixture.open();
        owner.start(0).unwrap();
        owner.verified(0).unwrap();
        owner.start(1).unwrap();
    }
    let view = fixture.read();
    assert_eq!(view.execution, OperationExecution::NeedsReceiptResolution);
    assert_eq!(view.stages[1].state, StageState::Started);
    assert!(view.needs_receipt_recheck);
}

#[test]
fn recorded_completion_still_requires_authoritative_receipt_and_runtime_probe() {
    let fixture = Fixture::new();
    let id = {
        let mut owner = fixture.open();
        owner.start(0).unwrap();
        owner.verified(0).unwrap();
        owner.start(1).unwrap();
        owner.verified(1).unwrap();
        owner.finish().unwrap();
        owner.operation_id()
    };
    let view = fixture.read();
    assert_eq!(view.execution, OperationExecution::RecordedComplete);
    assert_eq!(view.operation_id, id);
    assert!(view.needs_receipt_recheck);
    assert_eq!(view.models[0].asset_revision.as_deref(), Some("weights-v1"));
    let next = fixture.open();
    assert_ne!(next.operation_id(), id);
    assert_eq!(fixture.read().execution, OperationExecution::Running);
}

#[test]
fn inspection_of_torn_event_never_repairs_or_infers_transition() {
    let fixture = Fixture::new();
    {
        let mut owner = fixture.open();
        owner.start(0).unwrap();
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(fixture.journal())
        .unwrap();
    file.write_all(b"{\"event\":\"verified\"").unwrap();
    drop(file);
    let before = std::fs::read(fixture.journal()).unwrap();
    let view = fixture.read();
    assert!(view.journal_has_torn_tail);
    assert_eq!(view.stages[0].state, StageState::Started);
    assert_eq!(before, std::fs::read(fixture.journal()).unwrap());
    let reopened = fixture.open();
    assert_eq!(reopened.operation_id(), view.operation_id);
    drop(reopened);
    assert!(std::fs::metadata(fixture.journal()).unwrap().len() < before.len() as u64);
}

#[test]
fn complete_malformed_event_is_fail_closed() {
    let fixture = Fixture::new();
    drop(fixture.open());
    OpenOptions::new()
        .append(true)
        .open(fixture.journal())
        .unwrap()
        .write_all(b"{not-json}\n")
        .unwrap();
    let before = std::fs::read(fixture.journal()).unwrap();
    assert!(matches!(
        AiInstallOperationPresenter::new(fixture.root.clone()).read(&fixture.digest),
        Err(InstallOperationError::Corrupted)
    ));
    assert_eq!(before, std::fs::read(fixture.journal()).unwrap());
}

#[test]
fn changing_model_identity_fails_instead_of_relabeling_existing_operation() {
    let fixture = Fixture::new();
    drop(fixture.open());
    let mut changed = Fixture::models();
    changed[0].plugin_id = "other_asr".into();
    assert!(matches!(
        InstallOperation::open_described(
            &fixture.root,
            &fixture.digest,
            Fixture::stages(),
            changed
        ),
        Err(InstallOperationError::Invalid(_))
    ));
    assert_eq!(fixture.read().models[0].plugin_id, "asr_cpu");
}

#[cfg(unix)]
#[test]
fn symlinked_journal_is_refused_by_read_only_reader() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    drop(fixture.open());
    let journal = fixture.journal();
    let foreign = fixture.root.join("foreign");
    std::fs::write(&foreign, b"foreign-data").unwrap();
    std::fs::remove_file(&journal).unwrap();
    symlink(&foreign, &journal).unwrap();
    assert!(matches!(
        AiInstallOperationPresenter::new(fixture.root.clone()).read(&fixture.digest),
        Err(InstallOperationError::Corrupted)
    ));
    assert_eq!(std::fs::read(foreign).unwrap(), b"foreign-data");
}

#[test]
fn presenter_lists_existing_core_operations_without_fingerprint() {
    let fixture = Fixture::new();
    let presenter = AiInstallOperationPresenter::new(fixture.root.clone());
    assert!(presenter.list().unwrap().is_empty());
    let id = {
        let owner = fixture.open();
        let id = owner.operation_id();
        let views = presenter.list().unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].operation_id, id);
        assert_eq!(views[0].execution, OperationExecution::Running);
        assert_eq!(views[0].models, Fixture::models());
        id
    };
    let views = presenter.list().unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].operation_id, id);
    assert_eq!(views[0].execution, OperationExecution::Interrupted);
}

#[test]
fn inventory_fails_closed_if_any_operation_record_is_corrupt() {
    let fixture = Fixture::new();
    drop(fixture.open());
    OpenOptions::new()
        .append(true)
        .open(fixture.journal())
        .unwrap()
        .write_all(b"{broken}\n")
        .unwrap();
    assert!(matches!(
        AiInstallOperationPresenter::new(fixture.root.clone()).list(),
        Err(InstallOperationError::Corrupted)
    ));
}

#[test]
fn child_holds_real_os_lock_until_killed() {
    let Ok(root) = std::env::var("NM_OPERATION_VIEW_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut operation = InstallOperation::open_described(
        &root,
        &"9".repeat(64),
        Fixture::stages(),
        Fixture::models(),
    )
    .unwrap();
    operation.start(0).unwrap();
    std::fs::write(root.join("child-ready"), []).unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn real_process_kill_changes_running_to_interrupted_without_rewriting_journal() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let fixture = Fixture::new();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_holds_real_os_lock_until_killed"])
        .env("NM_OPERATION_VIEW_CHILD_ROOT", &fixture.root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !fixture.root.join("child-ready").exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not enter the journal-owned stage");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let active = fixture.read();
    assert_eq!(active.execution, OperationExecution::Running);
    let before = std::fs::read(fixture.journal()).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let interrupted = fixture.read();
    assert_eq!(interrupted.execution, OperationExecution::Interrupted);
    assert_eq!(interrupted.operation_id, active.operation_id);
    assert_eq!(interrupted.stages[0].state, StageState::Started);
    assert_eq!(before, std::fs::read(fixture.journal()).unwrap());
}

#[cfg(unix)]
#[test]
fn dangling_symlinked_operation_root_is_not_mistaken_for_absent_history() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    symlink(
        fixture.root.join("nonexistent"),
        fixture.root.join("operations"),
    )
    .unwrap();
    let presenter = AiInstallOperationPresenter::new(fixture.root.clone());
    assert!(matches!(
        presenter.list(),
        Err(InstallOperationError::Invalid(_))
    ));
    assert!(matches!(
        presenter.read(&fixture.digest),
        Err(InstallOperationError::Invalid(_))
    ));
}
