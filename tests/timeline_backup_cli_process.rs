#![cfg(feature = "sqlite-system")]

use std::{fs, path::PathBuf, process::Command};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        selection::TimelineSelectionRepository,
        storage::SqliteTimelineRepository,
    },
};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    directory: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    branch: BranchId,
    original: neuromita::timeline::EventId,
    latest: neuromita::timeline::EventId,
    keeper: Connection,
}

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("neuromita-timeline-backup-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("timeline.sqlite3");
        let destination = directory.join("backup.sqlite3");
        let branch = BranchId::new();
        let mut repo = SqliteTimelineRepository::open(&source).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        repo.select_branch(branch, None).unwrap();
        let keeper = Connection::open(&source).unwrap();
        keeper
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();

        let mut first = EventDraft::new(EventKind::UnityEvent);
        first.observers.insert(CharacterId::Crazy);
        let original = first.id;
        repo.append(
            branch,
            0,
            "initial",
            json!({"origin":"original"}),
            vec![first],
        )
        .unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        repo.append(branch, 1, "reset", json!({}), vec![reset])
            .unwrap();
        let mut last = EventDraft::new(EventKind::UnityEvent);
        last.observers.insert(CharacterId::Crazy);
        let latest = last.id;
        repo.append(branch, 2, "latest", json!({}), vec![last])
            .unwrap();
        assert!(
            fs::metadata(format!("{}-wal", source.display()))
                .unwrap()
                .len()
                > 0
        );
        Self {
            directory,
            source,
            destination,
            branch,
            original,
            latest,
            keeper,
        }
    }

    fn call(&self, output: &PathBuf) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .args([
                "timeline-backup",
                "create",
                "--database",
                self.source.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.keeper.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn canonical_snapshot(database: &PathBuf) -> Vec<Vec<String>> {
    let connection = Connection::open(database).unwrap();
    [
        "SELECT branch_id,session_id,ifnull(parent_id,''),fork_sequence,head_sequence,ifnull(checkpoint_id,'') FROM timeline_branches ORDER BY branch_id",
        "SELECT branch_id,sequence,fact_json FROM timeline_events ORDER BY branch_id,sequence",
        "SELECT branch_id,operation_key,receipt_json FROM timeline_receipts ORDER BY branch_id,operation_key",
        "SELECT slot,branch_id,generation FROM timeline_active_selection ORDER BY slot",
        "SELECT slot,revision FROM timeline_revision ORDER BY slot",
    ].iter().map(|sql| {
        let mut statement = connection.prepare(sql).unwrap();
        let fields = statement.column_count();
        statement.query_map([], |row| {
            let mut record = Vec::new();
            for i in 0..fields {
                let value = match row.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => String::new(),
                    rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                    rusqlite::types::ValueRef::Real(value) => value.to_string(),
                    rusqlite::types::ValueRef::Text(value) => String::from_utf8_lossy(value).to_string(),
                    rusqlite::types::ValueRef::Blob(value) => format!("{value:?}"),
                };
                record.push(value);
            }
            Ok(record.join("\u{1f}"))
        }).unwrap().map(Result::unwrap).collect()
    }).collect()
}

#[test]
fn backup_cli_includes_committed_wal_and_exact_timeline_canonical_state() {
    let fixture = Fixture::new();
    let source_state = canonical_snapshot(&fixture.source);
    let output = fixture.call(&fixture.destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("verified SQLite Timeline backup created")
    );
    assert_eq!(source_state, canonical_snapshot(&fixture.destination));
    let restored = SqliteTimelineRepository::open(&fixture.destination).unwrap();
    assert_eq!(
        restored
            .event(fixture.branch, fixture.original)
            .unwrap()
            .draft
            .id,
        fixture.original
    );
    assert_eq!(
        restored
            .event(fixture.branch, fixture.latest)
            .unwrap()
            .draft
            .id,
        fixture.latest
    );
    assert_eq!(
        restored.knows(fixture.branch, CharacterId::Crazy, fixture.original, None),
        Ok(false)
    );
    assert_eq!(
        restored.knows(fixture.branch, CharacterId::Crazy, fixture.latest, None),
        Ok(true)
    );
    let original = SqliteTimelineRepository::inspect_derived_indexes(&fixture.source).unwrap();
    let backup = SqliteTimelineRepository::inspect_derived_indexes(&fixture.destination).unwrap();
    assert_eq!(original, backup);
    assert!(!backup.requires_repair());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&fixture.destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn backup_cli_never_overwrites_an_existing_destination_or_source() {
    let fixture = Fixture::new();
    fs::write(&fixture.destination, b"important existing backup").unwrap();
    let before = canonical_snapshot(&fixture.source);
    let output = fixture.call(&fixture.destination);
    assert!(!output.status.success());
    assert_eq!(
        fs::read(&fixture.destination).unwrap(),
        b"important existing backup"
    );
    assert_eq!(canonical_snapshot(&fixture.source), before);
    let output = fixture.call(&fixture.source);
    assert!(!output.status.success());
    assert_eq!(canonical_snapshot(&fixture.source), before);
}

#[test]
fn backup_cli_refuses_corrupt_canonical_data_and_does_not_publish_output() {
    let fixture = Fixture::new();
    fixture
        .keeper
        .execute(
            "UPDATE timeline_events SET fact_json='broken' WHERE sequence=1",
            [],
        )
        .unwrap();
    let output = fixture.call(&fixture.destination);
    assert!(!output.status.success());
    assert!(!fixture.destination.exists());
    assert_eq!(
        fs::read_dir(&fixture.directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("neuromita-timeline-backup-"))
            .count(),
        0
    );
}

#[test]
fn backup_cli_refuses_missing_source_or_destination_directory() {
    let fixture = Fixture::new();
    let missing_source = fixture.directory.join("missing.sqlite3");
    let missing_directory = fixture.directory.join("not-created");
    let bad_output = missing_directory.join("backup.sqlite3");
    let first = Command::new(env!("CARGO_BIN_EXE_neuromita"))
        .args([
            "timeline-backup",
            "create",
            "--database",
            missing_source.to_str().unwrap(),
            "--output",
            fixture.destination.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!first.status.success());
    assert!(!missing_source.exists());
    assert!(!fixture.destination.exists());
    let second = fixture.call(&bad_output);
    assert!(!second.status.success());
    assert!(!bad_output.exists());
}

#[test]
fn backup_cli_refuses_damaged_derived_indexes_without_publishing_any_file() {
    let fixture = Fixture::new();
    fixture
        .keeper
        .execute("DELETE FROM timeline_observers WHERE sequence=1", [])
        .unwrap();
    let output = fixture.call(&fixture.destination);
    assert!(!output.status.success());
    assert!(!fixture.destination.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("need repair"));
}
