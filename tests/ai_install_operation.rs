use std::{fs::OpenOptions, io::Write};

use neuromita::ai_runtime::install_operation::{
    InstallOperation, InstallOperationError, OperationStage, StageState,
};
use uuid::Uuid;

fn fixture() -> (std::path::PathBuf, String, Vec<OperationStage>) {
    let root = std::env::temp_dir().join(format!("nm-operation-integration-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    (
        root,
        "b".repeat(64),
        vec![
            OperationStage {
                id: "tts/backend".into(),
                repeatable: true,
            },
            OperationStage {
                id: "tts/overlay".into(),
                repeatable: true,
            },
            OperationStage {
                id: "asr/generated".into(),
                repeatable: false,
            },
        ],
    )
}

#[test]
fn incomplete_stages_recover_with_same_id_but_cannot_claim_unverified_completion() {
    let (root, fingerprint, stages) = fixture();
    let operation_id = {
        let mut op = InstallOperation::open(&root, &fingerprint, stages.clone()).unwrap();
        op.start(0).unwrap();
        op.verified(0).unwrap();
        op.start(1).unwrap();
        assert!(op.finish().is_err());
        op.operation_id()
    };
    let mut recovered = InstallOperation::open(&root, &fingerprint, stages.clone()).unwrap();
    assert_eq!(recovered.operation_id(), operation_id);
    assert_eq!(recovered.stage_state(0).unwrap(), StageState::Verified);
    assert_eq!(recovered.stage_state(1).unwrap(), StageState::Started);
    assert_eq!(recovered.stage_state(2).unwrap(), StageState::Pending);
    recovered.start(1).unwrap();
    recovered.verified(1).unwrap();
    recovered.start(2).unwrap();
    drop(recovered);
    let mut again = InstallOperation::open(&root, &fingerprint, stages.clone()).unwrap();
    assert!(again.start(2).is_err());
    assert!(again.finish().is_err());
    drop(again);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_run_and_corruption_are_not_silent_fallbacks() {
    let (root, fingerprint, mut stages) = fixture();
    stages.truncate(1);
    let old_id = {
        let mut op = InstallOperation::open(&root, &fingerprint, stages.clone()).unwrap();
        assert!(matches!(
            InstallOperation::open(&root, &fingerprint, stages.clone()),
            Err(InstallOperationError::Busy)
        ));
        op.start(0).unwrap();
        op.verified(0).unwrap();
        op.finish().unwrap();
        op.operation_id()
    };
    let mut next = InstallOperation::open(&root, &fingerprint, stages.clone()).unwrap();
    assert_ne!(next.operation_id(), old_id);
    next.start(0).unwrap();
    drop(next);
    let journal = root.join(format!("operations/production/{fingerprint}.jsonl"));
    OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"{broken}\n")
        .unwrap();
    assert!(matches!(
        InstallOperation::open(&root, &fingerprint, stages),
        Err(InstallOperationError::Corrupted)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn torn_last_event_recovers_by_discarding_only_that_event() {
    let (root, fingerprint, stages) = fixture();
    let id = {
        let mut op = InstallOperation::open(&root, &fingerprint, stages.clone()).unwrap();
        op.start(0).unwrap();
        op.operation_id()
    };
    let journal = root.join(format!("operations/production/{fingerprint}.jsonl"));
    OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"{\"event\":\"verified\"")
        .unwrap();
    let op = InstallOperation::open(&root, &fingerprint, stages).unwrap();
    assert_eq!(op.operation_id(), id);
    assert_eq!(op.stage_state(0).unwrap(), StageState::Started);
    drop(op);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn operation_root_symlink_is_rejected_without_writing_into_target() {
    use std::os::unix::fs::symlink;
    let (root, fingerprint, stages) = fixture();
    let target = root.join("foreign");
    std::fs::create_dir_all(&target).unwrap();
    symlink(&target, root.join("operations")).unwrap();
    assert!(matches!(
        InstallOperation::open(&root, &fingerprint, stages),
        Err(InstallOperationError::Invalid(_))
    ));
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn operation_child_interrupted_after_publishing_first_stage() {
    let Ok(root) = std::env::var("NM_INSTALL_OPERATION_CHILD_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let ready = root.join("child-ready");
    let fingerprint = "f".repeat(64);
    let stages = vec![
        OperationStage {
            id: "backend".into(),
            repeatable: true,
        },
        OperationStage {
            id: "overlay".into(),
            repeatable: true,
        },
        OperationStage {
            id: "asset".into(),
            repeatable: false,
        },
    ];
    let mut op = InstallOperation::open(&root, &fingerprint, stages).unwrap();
    op.start(0).unwrap();
    std::fs::write(root.join("backend-receipt"), b"verified").unwrap();
    op.verified(0).unwrap();
    op.start(1).unwrap();
    std::fs::write(root.join("operation-id"), op.operation_id().to_string()).unwrap();
    std::fs::write(ready, []).unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn killed_process_reopens_same_operation_without_forging_completion() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let root = std::env::temp_dir().join(format!("nm-process-recovery-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "operation_child_interrupted_after_publishing_first_stage",
        ])
        .env("NM_INSTALL_OPERATION_CHILD_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ready = root.join("child-ready");
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready.exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not durably reach the interrupted installation stage");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let operation_id = std::fs::read_to_string(root.join("operation-id")).unwrap();
    assert_eq!(
        std::fs::read(root.join("backend-receipt")).unwrap(),
        b"verified"
    );
    let stages = vec![
        OperationStage {
            id: "backend".into(),
            repeatable: true,
        },
        OperationStage {
            id: "overlay".into(),
            repeatable: true,
        },
        OperationStage {
            id: "asset".into(),
            repeatable: false,
        },
    ];
    let mut recovered = InstallOperation::open(&root, &"f".repeat(64), stages).unwrap();
    assert_eq!(recovered.operation_id().to_string(), operation_id);
    assert_eq!(recovered.stage_state(0).unwrap(), StageState::Verified);
    assert_eq!(recovered.stage_state(1).unwrap(), StageState::Started);
    assert_eq!(recovered.stage_state(2).unwrap(), StageState::Pending);
    assert!(recovered.finish().is_err());
    recovered.start(1).unwrap();
    recovered.verified(1).unwrap();
    recovered.start(2).unwrap();
    assert!(recovered.finish().is_err());
    drop(recovered);
    std::fs::remove_dir_all(root).unwrap();
}
