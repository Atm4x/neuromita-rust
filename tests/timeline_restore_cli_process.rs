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
    backup: PathBuf,
    restored: PathBuf,
    branch: BranchId,
    first: neuromita::timeline::EventId,
    last: neuromita::timeline::EventId,
    keeper: Connection,
}

impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("neuromita-timeline-restore-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let source = directory.join("canonical.sqlite3");
        let backup = directory.join("verified-backup.sqlite3");
        let restored = directory.join("restored.sqlite3");
        let branch = BranchId::new();
        let mut repository = SqliteTimelineRepository::open(&source).unwrap();
        repository.create_session(SessionId::new(), branch).unwrap();
        repository.select_branch(branch, None).unwrap();
        let keeper = Connection::open(&source).unwrap();
        keeper
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();

        let mut first = EventDraft::new(EventKind::UnityEvent);
        first.observers.insert(CharacterId::Crazy);
        let first_id = first.id;
        repository
            .append(
                branch,
                0,
                "initial",
                json!({"operation":"first"}),
                vec![first],
            )
            .unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        repository
            .append(
                branch,
                1,
                "reset",
                json!({"operation":"reset"}),
                vec![reset],
            )
            .unwrap();
        let mut last = EventDraft::new(EventKind::UnityEvent);
        last.observers.insert(CharacterId::Crazy);
        let last_id = last.id;
        repository
            .append(branch, 2, "last", json!({"operation":"last"}), vec![last])
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
            backup,
            restored,
            branch,
            first: first_id,
            last: last_id,
            keeper,
        }
    }

    fn backup(&self) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .args([
                "timeline-backup",
                "create",
                "--database",
                self.source.to_str().unwrap(),
                "--output",
                self.backup.to_str().unwrap(),
            ])
            .env_remove("GEMINI_API_KEY")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("source fingerprint: ").map(str::to_owned))
            .expect("backup command must report exact fingerprint")
    }

    fn restore(&self, fingerprint: &str) -> std::process::Output {
        self.restore_to(&self.restored, fingerprint)
    }

    fn restore_to(&self, output: &PathBuf, fingerprint: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_neuromita"))
            .args([
                "timeline-restore",
                "into-new",
                "--backup",
                self.backup.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--confirm",
                fingerprint,
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
fn restore_cli_creates_independent_new_timeline_with_exact_ids_receipts_and_reset() {
    let fixture = Fixture::new();
    let fingerprint = fixture.backup();
    let source = canonical_snapshot(&fixture.source);
    let output = fixture.restore(&fingerprint);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = String::from_utf8_lossy(&output.stdout);
    assert!(result.contains("verified Timeline restored into new SQLite file"));
    assert!(result.contains(&fingerprint));
    assert_eq!(
        canonical_snapshot(&fixture.backup),
        canonical_snapshot(&fixture.restored)
    );
    assert_eq!(source, canonical_snapshot(&fixture.restored));
    let original_preview =
        SqliteTimelineRepository::inspect_derived_indexes(&fixture.backup).unwrap();
    let restored_preview =
        SqliteTimelineRepository::inspect_derived_indexes(&fixture.restored).unwrap();
    assert_eq!(original_preview, restored_preview);
    assert!(!restored_preview.requires_repair());

    fs::remove_file(&fixture.backup).unwrap();
    let restored = SqliteTimelineRepository::open(&fixture.restored).unwrap();
    assert_eq!(
        restored
            .event(fixture.branch, fixture.first)
            .unwrap()
            .draft
            .id,
        fixture.first
    );
    assert_eq!(
        restored
            .event(fixture.branch, fixture.last)
            .unwrap()
            .draft
            .id,
        fixture.last
    );
    assert_eq!(
        restored.knows(fixture.branch, CharacterId::Crazy, fixture.first, None),
        Ok(false)
    );
    assert_eq!(
        restored.knows(fixture.branch, CharacterId::Crazy, fixture.last, None),
        Ok(true)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&fixture.restored)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn restore_cli_rejects_wrong_fingerprint_and_does_not_create_destination() {
    let fixture = Fixture::new();
    let fingerprint = fixture.backup();
    let invalid = if fingerprint.starts_with('0') {
        "1"
    } else {
        "0"
    };
    let wrong = format!("{invalid}{}", &fingerprint[1..]);
    assert!(!fixture.restore(&wrong).status.success());
    assert!(!fixture.restored.exists());
    assert!(!fixture.restore("not-a-sha256").status.success());
    assert!(!fixture.restored.exists());
}

#[test]
fn restore_cli_rejects_stale_receipt_after_valid_backup_schema_change() {
    let fixture = Fixture::new();
    let fingerprint = fixture.backup();
    let connection = Connection::open(&fixture.backup).unwrap();
    connection
        .execute_batch("CREATE INDEX timeline_new_restore_schema ON timeline_events(sequence);")
        .unwrap();
    drop(connection);
    let changed = SqliteTimelineRepository::inspect_derived_indexes(&fixture.backup).unwrap();
    assert!(!changed.requires_repair());
    assert_ne!(changed.confirmation, fingerprint);
    assert!(!fixture.restore(&fingerprint).status.success());
    assert!(!fixture.restored.exists());
}

#[test]
fn restore_cli_rejects_mutated_or_corrupt_backup_before_publication() {
    let fixture = Fixture::new();
    let fingerprint = fixture.backup();
    let connection = Connection::open(&fixture.backup).unwrap();
    connection
        .execute("DELETE FROM timeline_observers WHERE sequence=1", [])
        .unwrap();
    drop(connection);
    assert!(!fixture.restore(&fingerprint).status.success());
    assert!(!fixture.restored.exists());
    fs::write(&fixture.backup, b"not a SQLite backup").unwrap();
    assert!(!fixture.restore(&fingerprint).status.success());
    assert!(!fixture.restored.exists());
}

#[test]
fn restore_cli_rejects_existing_output_source_and_missing_paths_without_clobber() {
    let fixture = Fixture::new();
    let fingerprint = fixture.backup();
    fs::write(&fixture.restored, b"existing Timeline must stay untouched").unwrap();
    assert!(!fixture.restore(&fingerprint).status.success());
    assert_eq!(
        fs::read(&fixture.restored).unwrap(),
        b"existing Timeline must stay untouched"
    );
    assert!(
        !fixture
            .restore_to(&fixture.backup, &fingerprint)
            .status
            .success()
    );
    assert!(SqliteTimelineRepository::inspect_derived_indexes(&fixture.backup).is_ok());
    let missing = fixture.directory.join("missing").join("restored.sqlite3");
    assert!(!fixture.restore_to(&missing, &fingerprint).status.success());
    assert!(!missing.exists());
}
