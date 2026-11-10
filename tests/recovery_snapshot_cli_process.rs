#![cfg(feature = "sqlite-system")]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::{
    recovery_snapshot,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        storage::SqliteTimelineRepository,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

struct Fixture {
    parent: PathBuf,
    source: PathBuf,
    snapshot: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let parent =
            std::env::temp_dir().join(format!("neuromita-snapshot-cli-{}", Uuid::new_v4()));
        fs::create_dir(&parent).unwrap();
        let source = parent.join("source");
        fs::create_dir(&source).unwrap();
        let mut repository =
            SqliteTimelineRepository::open(source.join("timeline.sqlite3")).unwrap();
        let branch = BranchId::new();
        repository.create_session(SessionId::new(), branch).unwrap();
        repository
            .append(
                branch,
                0,
                "snapshot-record",
                json!({"who":"operator"}),
                vec![EventDraft::new(EventKind::UnityEvent)],
            )
            .unwrap();
        drop(repository);
        fs::write(
            source.join("settings.json"),
            r#"{"secret":"not-for-stdout","version":1}"#,
        )
        .unwrap();
        fs::write(source.join("presets.json"), "[]").unwrap();
        let snapshot = parent.join("new-snapshot");
        Self {
            parent,
            source,
            snapshot,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .arg("recovery-snapshot")
            .args(args)
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    }

    fn create(&self) -> Output {
        self.run(&[
            "create",
            "--root",
            self.source.to_str().unwrap(),
            "--output",
            self.snapshot.to_str().unwrap(),
        ])
    }

    fn verify(&self) -> Output {
        self.run(&["verify", "--snapshot", self.snapshot.to_str().unwrap()])
    }

    fn assert_no_staging(&self) {
        for entry in fs::read_dir(&self.parent).unwrap() {
            assert!(
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".neuromita-recovery-")
            );
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

#[test]
fn exported_directory_survives_source_removal_with_exact_known_files() {
    let fixture = Fixture::new();
    let out = fixture.create();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("not-for-stdout"));
    let receipt = recovery_snapshot::verify(&fixture.snapshot).unwrap();
    assert_eq!(receipt.timeline.canonical_events, 1);
    assert_eq!(receipt.config_files.len(), 2);
    assert!(!receipt.complete_dataset_recovery);
    let inventory = serde_json::from_slice::<Value>(
        &fs::read(fixture.snapshot.join("recovery-manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        inventory["timeline"]["fingerprint"],
        receipt.timeline.fingerprint
    );
    fs::remove_dir_all(&fixture.source).unwrap();
    let out = fixture.verify();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fixture.assert_no_staging();
}

#[test]
fn existing_destinations_are_never_overwritten() {
    let fixture = Fixture::new();
    fs::write(&fixture.snapshot, b"foreign-owner").unwrap();
    assert!(!fixture.create().status.success());
    assert_eq!(fs::read(&fixture.snapshot).unwrap(), b"foreign-owner");
    fs::remove_file(&fixture.snapshot).unwrap();
    fs::create_dir(&fixture.snapshot).unwrap();
    fs::write(fixture.snapshot.join("sentinel"), "retain").unwrap();
    assert!(!fixture.create().status.success());
    assert_eq!(
        fs::read(fixture.snapshot.join("sentinel")).unwrap(),
        b"retain"
    );
    fixture.assert_no_staging();
}

#[test]
fn corrupted_inputs_fail_without_output_or_staging() {
    let fixture = Fixture::new();
    fs::write(fixture.source.join("settings.json"), "{not-json").unwrap();
    assert!(!fixture.create().status.success());
    assert!(!fixture.snapshot.exists());
    fs::write(fixture.source.join("settings.json"), "{}").unwrap();
    fs::write(fixture.source.join("timeline.sqlite3"), "not a database").unwrap();
    assert!(!fixture.create().status.success());
    assert!(!fixture.snapshot.exists());
    fixture.assert_no_staging();
}

#[test]
fn snapshot_reverification_rejects_tampering_and_extra_entries() {
    let fixture = Fixture::new();
    assert!(fixture.create().status.success());
    let data = fixture.snapshot.join("settings.json");
    let original = fs::read(&data).unwrap();
    fs::write(&data, "{}").unwrap();
    assert!(!fixture.verify().status.success());
    fs::write(&data, original).unwrap();
    assert!(fixture.verify().status.success());
    fs::write(fixture.snapshot.join("unexpected.json"), "{}").unwrap();
    assert!(!fixture.verify().status.success());
    fs::remove_file(fixture.snapshot.join("unexpected.json")).unwrap();
    assert!(fixture.verify().status.success());
    let manifest = fixture.snapshot.join("recovery-manifest.json");
    let mut content: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    content["complete_dataset_recovery"] = json!(true);
    fs::write(&manifest, serde_json::to_vec(&content).unwrap()).unwrap();
    assert!(!fixture.verify().status.success());
}

#[test]
fn snapshot_under_source_and_relative_paths_rejected() {
    let fixture = Fixture::new();
    let destination = fixture.source.join("inside-source");
    assert!(
        !fixture
            .run(&[
                "create",
                "--root",
                fixture.source.to_str().unwrap(),
                "--output",
                destination.to_str().unwrap()
            ])
            .status
            .success()
    );
    assert!(!destination.exists());
    assert!(
        !fixture
            .run(&[
                "create",
                "--root",
                "relative",
                "--output",
                fixture.snapshot.to_str().unwrap()
            ])
            .status
            .success()
    );
    fixture.assert_no_staging();
}

#[cfg(unix)]
#[test]
fn output_symlink_refused_and_real_target_intact() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let foreign = fixture.parent.join("foreign");
    fs::create_dir(&foreign).unwrap();
    symlink(&foreign, &fixture.snapshot).unwrap();
    assert!(!fixture.create().status.success());
    assert!(
        fs::symlink_metadata(&fixture.snapshot)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_dir(&foreign).unwrap().count(), 0);
    fixture.assert_no_staging();
}
