#![cfg(feature = "sqlite-system")]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::{
    recovery_inventory::{self, InventoryError},
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        storage::SqliteTimelineRepository,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    manifest: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("neuromita-recovery-inventory-{}", Uuid::new_v4()));
        let root = directory.join("data");
        fs::create_dir_all(&root).unwrap();
        let mut repo = SqliteTimelineRepository::open(root.join("timeline.sqlite3")).unwrap();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        repo.append(
            branch,
            0,
            "initial",
            json!({"origin":"inventory"}),
            vec![EventDraft::new(EventKind::UnityEvent)],
        )
        .unwrap();
        drop(repo);
        fs::write(
            root.join("settings.json"),
            r#"{"sensitive":"not-for-stdout","enabled":true}"#,
        )
        .unwrap();
        fs::write(root.join("presets.json"), r#"{"presets":[]}"#).unwrap();
        let manifest = directory.join("saved-manifest.json");
        Self {
            directory,
            root,
            manifest,
        }
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .arg("recovery-inventory")
            .args(arguments)
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    }

    fn capture(&self) -> Value {
        let out = self.run(&["inspect", "--root", self.root.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("not-for-stdout"));
        fs::write(&self.manifest, &out.stdout).unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn verify(&self) -> Output {
        self.run(&[
            "verify",
            "--root",
            self.root.to_str().unwrap(),
            "--manifest",
            self.manifest.to_str().unwrap(),
        ])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn real_cli_inventory_and_read_only_reverification() {
    let fixture = Fixture::new();
    let result = fixture.capture();
    assert_eq!(result["version"], 1);
    assert_eq!(result["complete_dataset_recovery"], false);
    assert_eq!(result["timeline"]["canonical_events"], 1);
    assert_eq!(result["config_files"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["missing_optional_config_files"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(result["unverified_owners"].as_array().unwrap().len(), 4);
    assert!(fixture.verify().status.success());
    assert_eq!(
        fs::read_to_string(fixture.root.join("settings.json")).unwrap(),
        r#"{"sensitive":"not-for-stdout","enabled":true}"#
    );
}

#[test]
fn changed_config_and_presence_changes_fail_closed() {
    let fixture = Fixture::new();
    fixture.capture();
    fs::write(
        fixture.root.join("settings.json"),
        r#"{"sensitive":"altered"}"#,
    )
    .unwrap();
    assert!(!fixture.verify().status.success());
    fs::write(
        fixture.root.join("settings.json"),
        r#"{"sensitive":"not-for-stdout","enabled":true}"#,
    )
    .unwrap();
    assert!(fixture.verify().status.success());
    fs::write(fixture.root.join("finetune.json"), r#"{}"#).unwrap();
    assert!(!fixture.verify().status.success());
}

#[test]
fn unsupported_manifest_or_tampered_timeline_evidence_rejected() {
    let fixture = Fixture::new();
    let mut result = fixture.capture();
    result["complete_dataset_recovery"] = json!(true);
    fs::write(&fixture.manifest, serde_json::to_vec(&result).unwrap()).unwrap();
    assert!(!fixture.verify().status.success());
    assert!(matches!(
        recovery_inventory::parse_manifest(&serde_json::to_vec(&result).unwrap()),
        Err(InventoryError::InvalidJson(_))
    ));
    result["complete_dataset_recovery"] = json!(false);
    result["timeline"]["fingerprint"] = json!("0".repeat(64));
    fs::write(&fixture.manifest, serde_json::to_vec(&result).unwrap()).unwrap();
    assert!(!fixture.verify().status.success());
    result["unadvertised_key"] = json!(1);
    assert!(matches!(
        recovery_inventory::parse_manifest(&serde_json::to_vec(&result).unwrap()),
        Err(InventoryError::InvalidJson(_))
    ));
}

#[test]
fn corrupted_config_and_corrupted_timeline_rejected_without_repair() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("presets.json"), "not json").unwrap();
    assert!(
        !fixture
            .run(&["inspect", "--root", fixture.root.to_str().unwrap()])
            .status
            .success()
    );
    fs::write(fixture.root.join("presets.json"), "{}").unwrap();
    fs::write(fixture.root.join("timeline.sqlite3"), "not SQLite").unwrap();
    assert!(
        !fixture
            .run(&["inspect", "--root", fixture.root.to_str().unwrap()])
            .status
            .success()
    );
}

#[test]
fn missing_database_and_relative_paths_rejected() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("timeline.sqlite3")).unwrap();
    assert!(
        !fixture
            .run(&["inspect", "--root", fixture.root.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&["inspect", "--root", "relative"])
            .status
            .success()
    );
}

#[cfg(unix)]
#[test]
fn symlinked_known_config_file_rejected() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("presets.json")).unwrap();
    symlink(
        fixture.root.join("settings.json"),
        fixture.root.join("presets.json"),
    )
    .unwrap();
    assert!(
        !fixture
            .run(&["inspect", "--root", fixture.root.to_str().unwrap()])
            .status
            .success()
    );
}
