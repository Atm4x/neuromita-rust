#![cfg(feature = "sqlite-system")]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::{
    character::CharacterId,
    recovery_inventory, recovery_snapshot,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        selection::TimelineSelectionRepository,
        storage::SqliteTimelineRepository,
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use uuid::Uuid;

struct Fixture {
    parent: PathBuf,
    source: PathBuf,
    snapshot: PathBuf,
    restored: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let parent =
            std::env::temp_dir().join(format!("neuromita-recovery-restore-cli-{}", Uuid::new_v4()));
        let source = parent.join("original-data");
        let snapshot = parent.join("sealed-snapshot");
        let restored = parent.join("restored-data");
        fs::create_dir_all(&source).unwrap();
        let sqlite = source.join("timeline.sqlite3");
        let branch = BranchId::new();
        let mut repository = SqliteTimelineRepository::open(&sqlite).unwrap();
        repository.create_session(SessionId::new(), branch).unwrap();
        repository.select_branch(branch, None).unwrap();
        let keeper = Connection::open(&sqlite).unwrap();
        keeper
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        for (sequence, kind, operation) in [
            (0, EventKind::UnityEvent, "before-reset"),
            (1, EventKind::CharacterContextReset, "context-reset"),
            (2, EventKind::UnityEvent, "after-reset"),
        ] {
            let mut draft = EventDraft::new(kind);
            draft.observers.insert(CharacterId::Crazy);
            repository
                .append(
                    branch,
                    sequence,
                    operation,
                    json!({"seq":sequence}),
                    vec![draft],
                )
                .unwrap();
        }
        assert!(
            fs::metadata(format!("{}-wal", sqlite.display()))
                .unwrap()
                .len()
                > 0
        );
        drop(repository);
        drop(keeper);
        fs::write(
            source.join("settings.json"),
            r#"{"secret":"not-for-stdout","language":"ru"}"#,
        )
        .unwrap();
        fs::write(source.join("presets.json"), "[]").unwrap();
        fs::write(
            source.join("preset-credentials.json"),
            r#"{"key":"private"}"#,
        )
        .unwrap();
        let fixture = Self {
            parent,
            source,
            snapshot,
            restored,
        };
        fixture
            .command(&[
                "create",
                "--root",
                fixture.source.to_str().unwrap(),
                "--output",
                fixture.snapshot.to_str().unwrap(),
            ])
            .assert_success();
        fixture
    }

    fn command(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .arg("recovery-snapshot")
            .args(args)
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    }

    fn restore_to(&self, output: &PathBuf) -> Output {
        self.command(&[
            "restore",
            "--snapshot",
            self.snapshot.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
    }

    fn assert_no_staging(&self) {
        assert!(fs::read_dir(&self.parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".neuromita-recovery-")
        }));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.parent);
    }
}

trait AssertSuccess {
    fn assert_success(&self);
}
impl AssertSuccess for Output {
    fn assert_success(&self) {
        assert!(
            self.status.success(),
            "{}",
            String::from_utf8_lossy(&self.stderr)
        );
    }
}

fn canonical_rows(path: &PathBuf) -> Vec<Vec<String>> {
    let db = Connection::open(path).unwrap();
    [
        "SELECT branch_id,session_id,ifnull(parent_id,''),fork_sequence,head_sequence,ifnull(checkpoint_id,'') FROM timeline_branches ORDER BY branch_id",
        "SELECT branch_id,sequence,fact_json FROM timeline_events ORDER BY branch_id,sequence",
        "SELECT branch_id,operation_key,receipt_json FROM timeline_receipts ORDER BY branch_id,operation_key",
        "SELECT slot,branch_id,generation FROM timeline_active_selection ORDER BY slot",
        "SELECT slot,revision FROM timeline_revision ORDER BY slot",
    ]
    .iter()
    .map(|sql| {
        let mut statement = db.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement.query_map([], |row| {
            (0..columns).map(|i| {
                let item = row.get_ref(i)?;
                Ok::<_, rusqlite::Error>(match item {
                    rusqlite::types::ValueRef::Null => String::new(),
                    rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                    rusqlite::types::ValueRef::Real(n) => n.to_string(),
                    rusqlite::types::ValueRef::Text(b) => String::from_utf8_lossy(b).into_owned(),
                    rusqlite::types::ValueRef::Blob(b) => format!("{b:?}"),
                })
            }).collect::<Result<Vec<_>, _>>()
        }).unwrap().map(Result::unwrap).map(|row| row.join("\u{1f}")).collect()
    }).collect()
}

#[test]
fn standalone_restore_preserves_event_ids_receipts_selection_and_config_independence() {
    let fixture = Fixture::new();
    let expected = recovery_snapshot::verify(&fixture.snapshot).unwrap();
    let original_rows = canonical_rows(&fixture.source.join("timeline.sqlite3"));
    assert_eq!(expected.timeline.canonical_events, 3);
    assert_eq!(expected.config_files.len(), 3);
    fs::remove_dir_all(&fixture.source).unwrap();
    let result = fixture.restore_to(&fixture.restored);
    result.assert_success();
    assert!(!String::from_utf8_lossy(&result.stdout).contains("not-for-stdout"));
    assert!(!String::from_utf8_lossy(&result.stdout).contains("private"));
    assert!(String::from_utf8_lossy(&result.stdout).contains("complete dataset recovery: false"));
    assert_eq!(
        canonical_rows(&fixture.restored.join("timeline.sqlite3")),
        original_rows
    );
    assert_eq!(
        recovery_inventory::inspect(&fixture.restored).unwrap(),
        expected
    );
    assert!(!fixture.restored.join("recovery-manifest.json").exists());
    assert!(!fixture.restored.join("finetune.json").exists());
    assert_eq!(
        fs::read(fixture.restored.join("settings.json")).unwrap(),
        fs::read(fixture.snapshot.join("settings.json")).unwrap()
    );
    fs::write(
        fixture.restored.join("settings.json"),
        r#"{"language":"en"}"#,
    )
    .unwrap();
    assert!(recovery_snapshot::verify(&fixture.snapshot).is_ok());
    assert!(
        fs::read(fixture.snapshot.join("settings.json"))
            .unwrap()
            .windows(14)
            .any(|w| w == b"not-for-stdout")
    );
    fixture.assert_no_staging();
}

#[test]
fn damaged_manifest_and_wrong_fingerprint_refuse_without_publication() {
    let fixture = Fixture::new();
    let manifest = fixture.snapshot.join("recovery-manifest.json");
    let original = fs::read(&manifest).unwrap();
    let mut value: Value = serde_json::from_slice(&original).unwrap();
    value["timeline"]["fingerprint"] = json!("0".repeat(64));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert!(!fixture.restored.exists());
    fs::write(&manifest, "{ broken").unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert!(!fixture.restored.exists());
    fs::write(&manifest, original).unwrap();
    fixture.restore_to(&fixture.restored).assert_success();
    fixture.assert_no_staging();
}

#[test]
fn missing_or_modified_config_and_unexpected_entry_refuse() {
    let fixture = Fixture::new();
    let settings = fixture.snapshot.join("settings.json");
    let data = fs::read(&settings).unwrap();
    fs::remove_file(&settings).unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert!(!fixture.restored.exists());
    fs::write(&settings, b"{}").unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert!(!fixture.restored.exists());
    fs::write(&settings, data).unwrap();
    fs::write(fixture.snapshot.join("alien.txt"), "unclaimed").unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert!(!fixture.restored.exists());
    fixture.assert_no_staging();
}

#[test]
fn existing_destination_file_and_directory_are_never_overwritten() {
    let fixture = Fixture::new();
    fs::write(&fixture.restored, "other owner's file").unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert_eq!(fs::read(&fixture.restored).unwrap(), b"other owner's file");
    fs::remove_file(&fixture.restored).unwrap();
    fs::create_dir(&fixture.restored).unwrap();
    fs::write(fixture.restored.join("sentinel"), "protected").unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert_eq!(
        fs::read(fixture.restored.join("sentinel")).unwrap(),
        b"protected"
    );
    fixture.assert_no_staging();
}

#[test]
fn relative_output_and_restore_inside_snapshot_are_rejected() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .command(&[
                "restore",
                "--snapshot",
                fixture.snapshot.to_str().unwrap(),
                "--output",
                "relative"
            ])
            .status
            .success()
    );
    let child = fixture.snapshot.join("inside");
    assert!(!fixture.restore_to(&child).status.success());
    assert!(!child.exists());
    assert!(recovery_snapshot::verify(&fixture.snapshot).is_ok());
    fixture.assert_no_staging();
}

#[cfg(unix)]
#[test]
fn destination_symlink_is_rejected_without_touching_its_target() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let protected = fixture.parent.join("protected");
    fs::create_dir(&protected).unwrap();
    fs::write(protected.join("sentinel"), "not mine").unwrap();
    symlink(&protected, &fixture.restored).unwrap();
    assert!(!fixture.restore_to(&fixture.restored).status.success());
    assert_eq!(fs::read(protected.join("sentinel")).unwrap(), b"not mine");
    fixture.assert_no_staging();
}
