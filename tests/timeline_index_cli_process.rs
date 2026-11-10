#![cfg(feature = "sqlite-system")]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
        storage::SqliteTimelineRepository,
    },
};
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

struct Fixture {
    path: PathBuf,
    branch: BranchId,
    old_event: neuromita::timeline::EventId,
    recent_event: neuromita::timeline::EventId,
}

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "neuromita-cli-index-maintenance-{}.sqlite3",
            Uuid::new_v4()
        ));
        let branch = BranchId::new();
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut old = EventDraft::new(EventKind::UnityEvent);
        old.observers.insert(CharacterId::Crazy);
        let old_event = old.id;
        repo.append(
            branch,
            0,
            "original",
            json!({"origin":"canonical"}),
            vec![old],
        )
        .unwrap();
        let mut reset = EventDraft::new(EventKind::CharacterContextReset);
        reset.observers.insert(CharacterId::Crazy);
        repo.append(branch, 1, "reset", json!({}), vec![reset])
            .unwrap();
        let mut recent = EventDraft::new(EventKind::UnityEvent);
        recent.observers.insert(CharacterId::Crazy);
        let recent_event = recent.id;
        repo.append(branch, 2, "recent", json!({}), vec![recent])
            .unwrap();
        assert!(
            !repo
                .knows(branch, CharacterId::Crazy, old_event, None)
                .unwrap()
        );
        assert!(
            repo.knows(branch, CharacterId::Crazy, recent_event, None)
                .unwrap()
        );
        Self {
            path,
            branch,
            old_event,
            recent_event,
        }
    }

    fn db(&self) -> Connection {
        Connection::open(&self.path).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!("{}{}", self.path.display(), suffix));
        }
    }
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_neuromita"))
        .args(args)
        .env_remove("GEMINI_API_KEY")
        .output()
        .unwrap()
}

fn inspect(path: &Path) -> String {
    let output = cli(&[
        "timeline-index",
        "inspect",
        "--database",
        path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fingerprint(report: &str) -> String {
    let fingerprint = report
        .lines()
        .find_map(|line| line.strip_prefix("confirmation fingerprint: "))
        .expect("CLI must report fingerprint");
    assert_eq!(fingerprint.len(), 64);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
    fingerprint.to_owned()
}

fn repair(path: &Path, token: &str) -> Output {
    cli(&[
        "timeline-index",
        "repair",
        "--database",
        path.to_str().unwrap(),
        "--confirm",
        token,
    ])
}

fn canonical_snapshot(connection: &Connection) -> Vec<Vec<String>> {
    [
        "SELECT branch_id,session_id,ifnull(parent_id,''),fork_sequence,head_sequence,ifnull(checkpoint_id,'') FROM timeline_branches ORDER BY branch_id",
        "SELECT branch_id,sequence,fact_json FROM timeline_events ORDER BY branch_id,sequence",
        "SELECT branch_id,operation_key,receipt_json FROM timeline_receipts ORDER BY branch_id,operation_key",
        "SELECT slot,branch_id,generation FROM timeline_active_selection ORDER BY slot",
        "SELECT slot,revision FROM timeline_revision ORDER BY slot",
    ]
    .iter()
    .map(|sql| {
        let mut statement = connection.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                let mut values = Vec::new();
                for index in 0..columns {
                    let value = match row.get_ref(index)? {
                        rusqlite::types::ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                        rusqlite::types::ValueRef::Real(value) => value.to_string(),
                        rusqlite::types::ValueRef::Null => String::new(),
                        rusqlite::types::ValueRef::Blob(bytes) => format!("{bytes:?}"),
                    };
                    values.push(value);
                }
                Ok(values.join("\u{1f}"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    })
    .collect()
}

#[test]
fn standalone_cli_inspects_repairs_and_reopens_without_modifying_canonical_timeline() {
    let fixture = Fixture::new();
    let initial_canonical = canonical_snapshot(&fixture.db());
    let db = fixture.db();
    db.execute_batch(
        "DROP TABLE timeline_fact_keys;
         DELETE FROM timeline_observers;
         DELETE FROM timeline_resets;",
    )
    .unwrap();
    drop(db);

    let damaged = inspect(&fixture.path);
    assert!(damaged.contains("repair needed: true"));
    assert!(damaged.contains("timeline_fact_keys: exists=false"));
    assert!(damaged.contains("timeline_resets: exists=true"));
    let token = fingerprint(&damaged);
    let db = fixture.db();
    assert_eq!(
        db.query_row("SELECT count(*) FROM timeline_resets", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='timeline_fact_keys'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(canonical_snapshot(&db), initial_canonical);
    drop(db);

    let denied = repair(&fixture.path, &"0".repeat(64));
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stdout).contains("repair completed"));
    let restored = repair(&fixture.path, &token);
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let restored_out = String::from_utf8(restored.stdout).unwrap();
    assert!(restored_out.contains("repair completed"));
    assert!(restored_out.contains("repair needed: false"));

    let reopened = SqliteTimelineRepository::open(&fixture.path).unwrap();
    assert_eq!(
        reopened
            .event(fixture.branch, fixture.old_event)
            .unwrap()
            .draft
            .id,
        fixture.old_event
    );
    assert_eq!(
        reopened
            .event(fixture.branch, fixture.recent_event)
            .unwrap()
            .draft
            .id,
        fixture.recent_event
    );
    assert!(
        !reopened
            .knows(fixture.branch, CharacterId::Crazy, fixture.old_event, None)
            .unwrap()
    );
    assert!(
        reopened
            .knows(
                fixture.branch,
                CharacterId::Crazy,
                fixture.recent_event,
                None
            )
            .unwrap()
    );
    assert_eq!(
        reopened
            .read(fixture.branch, 0, None, 10, Some(CharacterId::Crazy))
            .unwrap()
            .len(),
        3
    );
    assert_eq!(canonical_snapshot(&fixture.db()), initial_canonical);
    assert!(inspect(&fixture.path).contains("repair needed: false"));
}

#[test]
fn standalone_cli_rejects_stale_source_and_index_confirmations() {
    let fixture = Fixture::new();
    let db = fixture.db();
    db.execute("DELETE FROM timeline_observers WHERE sequence=1", [])
        .unwrap();
    drop(db);
    let first = fingerprint(&inspect(&fixture.path));
    let mut repo = SqliteTimelineRepository::open(&fixture.path).unwrap();
    let mut new_event = EventDraft::new(EventKind::UnityEvent);
    new_event.observers.insert(CharacterId::Kind);
    repo.append(fixture.branch, 3, "concurrent", json!({}), vec![new_event])
        .unwrap();
    let failed = repair(&fixture.path, &first);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("error:"));
    let second = fingerprint(&inspect(&fixture.path));
    fixture
        .db()
        .execute("DELETE FROM timeline_fact_keys WHERE sequence=1", [])
        .unwrap();
    let denied = repair(&fixture.path, &second);
    assert!(!denied.status.success());
    assert!(inspect(&fixture.path).contains("repair needed: true"));
}

#[test]
fn standalone_cli_rejects_malformed_source_and_never_creates_missing_database() {
    let fixture = Fixture::new();
    let db = fixture.db();
    db.execute(
        "UPDATE timeline_events SET fact_json='{}' WHERE sequence=1",
        [],
    )
    .unwrap();
    drop(db);
    let report = cli(&[
        "timeline-index",
        "inspect",
        "--database",
        fixture.path.to_str().unwrap(),
    ]);
    assert!(!report.status.success());
    let denied = repair(&fixture.path, &"0".repeat(64));
    assert!(!denied.status.success());
    let missing = fixture.path.with_extension("nonexistent.sqlite3");
    assert!(!missing.exists());
    let not_found = cli(&[
        "timeline-index",
        "inspect",
        "--database",
        missing.to_str().unwrap(),
    ]);
    assert!(!not_found.status.success());
    assert!(!missing.exists());
}

#[test]
fn standalone_cli_rejects_timeline_schema_change_after_confirmation() {
    let fixture = Fixture::new();
    let db = fixture.db();
    db.execute("DELETE FROM timeline_observers WHERE sequence=1", [])
        .unwrap();
    drop(db);
    let token = fingerprint(&inspect(&fixture.path));
    let db = fixture.db();
    db.execute_batch("CREATE INDEX timeline_events_secondary_probe ON timeline_events(sequence)")
        .unwrap();
    drop(db);

    let rejected = repair(&fixture.path, &token);
    assert!(
        !rejected.status.success(),
        "a confirmation issued before SQLite schema changes must be stale"
    );
    assert!(inspect(&fixture.path).contains("repair needed: true"));
}

#[test]
fn standalone_cli_rolls_back_when_database_trigger_prevents_repair_convergence() {
    let fixture = Fixture::new();
    let db = fixture.db();
    db.execute("DELETE FROM timeline_observers WHERE sequence=1", [])
        .unwrap();
    db.execute_batch(
        "CREATE TRIGGER derived_observer_pollution
         AFTER INSERT ON timeline_observers
         BEGIN
           INSERT OR IGNORE INTO timeline_observers(branch_id,sequence,character_id)
           VALUES(NEW.branch_id,NEW.sequence,'phantom');
         END",
    )
    .unwrap();
    let original_count: i64 = db
        .query_row("SELECT COUNT(*) FROM timeline_observers", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(db);

    let report = inspect(&fixture.path);
    assert!(report.contains("repair needed: true"));
    let rejected = repair(&fixture.path, &fingerprint(&report));
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("did not converge"));
    let db = fixture.db();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM timeline_observers", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        original_count
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM timeline_observers WHERE character_id='phantom'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(inspect(&fixture.path).contains("repair needed: true"));
}
