use std::{collections::BTreeSet, path::Path};

use ring::digest::{SHA256, digest};

use super::{Connection, SCHEMA, SqliteTimelineRepository, index_fact, load, revision, storage};
use crate::timeline::domain::TimelineError;

const MAX_PREVIEW_EVENTS: usize = 100_000;
const MAX_PREVIEW_INDEX_ROWS: usize = 500_000;
const MAX_PREVIEW_SCHEMA_OBJECTS: usize = 10_000;
const TABLES: [(&str, &str); 4] = [
    (
        "timeline_fact_keys",
        "SELECT branch_id,sequence,event_id,quote(external_id),kind_json FROM timeline_fact_keys ORDER BY branch_id,sequence",
    ),
    (
        "timeline_observers",
        "SELECT branch_id,sequence,character_id FROM timeline_observers ORDER BY branch_id,sequence,character_id",
    ),
    (
        "timeline_acquisitions",
        "SELECT branch_id,sequence,character_id,event_id FROM timeline_acquisitions ORDER BY branch_id,sequence,character_id,event_id",
    ),
    (
        "timeline_resets",
        "SELECT branch_id,sequence,character_id FROM timeline_resets ORDER BY branch_id,sequence,character_id",
    ),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedIndexDrift {
    pub table: &'static str,
    pub exists: bool,
    pub expected_rows: usize,
    pub actual_rows: usize,
    pub missing_rows: usize,
    pub unexpected_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedIndexPreview {
    pub revision: u64,
    pub canonical_events: usize,
    pub drift: Vec<DerivedIndexDrift>,
    /// Optimistic, content-bound confirmation. Not an authentication credential.
    pub confirmation: String,
}

impl DerivedIndexPreview {
    pub fn requires_repair(&self) -> bool {
        self.drift
            .iter()
            .any(|table| !table.exists || table.missing_rows > 0 || table.unexpected_rows > 0)
    }
}

impl SqliteTimelineRepository {
    /// Inspect an existing database without schema initialization or persistent writes.
    /// The caller must be a trusted local operator; this does not open an application.
    pub fn inspect_derived_indexes(
        path: impl AsRef<Path>,
    ) -> Result<DerivedIndexPreview, TimelineError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(storage("timeline database must already exist"));
        }
        let connection = Connection::open_read_only(path)?;
        connection.exec("BEGIN")?;
        let result = preview_in_transaction(&connection);
        connection.exec("ROLLBACK")?;
        result
    }

    /// Apply only the inspected repair while holding the SQLite writer lock.
    /// Source and derived index state must still match the previously inspected token.
    pub fn rebuild_derived_indexes_confirmed(
        path: impl AsRef<Path>,
        confirmation: &str,
    ) -> Result<DerivedIndexPreview, TimelineError> {
        if confirmation.len() != 64 || !confirmation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(TimelineError::Invalid("invalid derived-index confirmation"));
        }
        let path = path.as_ref();
        if !path.is_file() {
            return Err(storage("timeline database must already exist"));
        }
        let connection = Connection::open_existing(path)?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let before = preview_in_transaction(&connection)?;
            if before.confirmation != confirmation {
                return Err(TimelineError::ConcurrentWrite);
            }
            if before.requires_repair() {
                connection.exec(SCHEMA)?;
                rebuild_in_transaction(&connection)?;
            }
            let after = preview_in_transaction(&connection)?;
            if after.requires_repair() {
                return Err(storage("derived-index repair did not converge"));
            }
            Ok(after)
        })();
        match result {
            Ok(after) => {
                connection.exec("COMMIT")?;
                Ok(after)
            }
            Err(error) => {
                connection.exec("ROLLBACK")?;
                Err(error)
            }
        }
    }
}

pub(super) fn rebuild_in_transaction(connection: &Connection) -> Result<(), TimelineError> {
    let store = load(connection)?;
    connection.exec(
        "DELETE FROM timeline_fact_keys;
         DELETE FROM timeline_observers;
         DELETE FROM timeline_acquisitions;
         DELETE FROM timeline_resets;",
    )?;
    populate_indexes(connection, &store)
}

fn populate_indexes(
    connection: &Connection,
    store: &crate::timeline::domain::TimelineStore,
) -> Result<(), TimelineError> {
    for branch in store.persisted_branches() {
        for fact in &branch.events {
            index_fact(connection, fact)?;
            for observer in &fact.draft.observers {
                connection.write(
                    "INSERT INTO timeline_observers(branch_id,sequence,character_id) VALUES(?,?,?)",
                    &[
                        &fact.branch_id.0.to_string(),
                        &fact.sequence.to_string(),
                        &observer.to_string(),
                    ],
                )?;
            }
        }
    }
    Ok(())
}

fn preview_in_transaction(connection: &Connection) -> Result<DerivedIndexPreview, TimelineError> {
    let versions = connection.query("SELECT version FROM timeline_schema", &[])?;
    if versions != [vec!["2".to_owned()]] {
        return Err(storage("unsupported timeline schema version"));
    }
    let store = load(connection)?;
    let canonical_events: usize = store
        .persisted_branches()
        .into_iter()
        .map(|branch| branch.events.len())
        .sum();
    if canonical_events > MAX_PREVIEW_EVENTS {
        return Err(TimelineError::Invalid(
            "derived-index preview event limit exceeded",
        ));
    }
    let count = |table: &str| -> Result<usize, TimelineError> {
        let rows = connection.query(&format!("SELECT COUNT(*) FROM {table}"), &[])?;
        let [row] = rows.as_slice() else {
            return Err(storage("invalid canonical row count"));
        };
        row[0]
            .parse::<usize>()
            .map_err(|_| storage("invalid canonical row count"))
    };
    let receipt_count: usize = store
        .persisted_branches()
        .iter()
        .map(|branch| branch.receipts.len())
        .sum();
    if count("timeline_events")? != canonical_events || count("timeline_receipts")? != receipt_count
    {
        return Err(storage("orphan canonical timeline event or receipt"));
    }
    let scratch = Connection::scratch()?;
    scratch.exec(SCHEMA)?;
    populate_indexes(&scratch, &store)?;

    let mut fingerprint_rows = Vec::new();
    for (label, sql) in [
        (
            "branches",
            "SELECT branch_id,session_id,ifnull(parent_id,''),fork_sequence,head_sequence,ifnull(checkpoint_id,'') FROM timeline_branches ORDER BY branch_id",
        ),
        (
            "events",
            "SELECT branch_id,sequence,fact_json FROM timeline_events ORDER BY branch_id,sequence",
        ),
        (
            "receipts",
            "SELECT branch_id,operation_key,receipt_json FROM timeline_receipts ORDER BY branch_id,operation_key",
        ),
        (
            "selection",
            "SELECT slot,branch_id,generation FROM timeline_active_selection ORDER BY slot",
        ),
        (
            "revision",
            "SELECT slot,revision FROM timeline_revision ORDER BY slot",
        ),
    ] {
        fingerprint_rows.push((label, true, connection.query(sql, &[])?));
    }
    let schema_objects = connection.query(
        "SELECT type,name,tbl_name,ifnull(sql,'') FROM sqlite_master
         WHERE name GLOB 'timeline_*' OR tbl_name GLOB 'timeline_*'
         ORDER BY type,name,tbl_name",
        &[],
    )?;
    if schema_objects.len() > MAX_PREVIEW_SCHEMA_OBJECTS {
        return Err(TimelineError::Invalid(
            "derived-index preview schema object limit exceeded",
        ));
    }
    fingerprint_rows.push(("schema_objects", true, schema_objects));
    let mut drift = Vec::new();
    for (table, sql) in TABLES {
        let exists = !connection
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
                &[table],
            )?
            .is_empty();
        if exists {
            let rows = connection.query(&format!("SELECT COUNT(*) FROM {table}"), &[])?;
            let count = rows[0][0]
                .parse::<usize>()
                .map_err(|_| storage("invalid derived row count"))?;
            if count > MAX_PREVIEW_INDEX_ROWS {
                return Err(TimelineError::Invalid("derived-index row limit exceeded"));
            }
        }
        let actual = if exists {
            connection.query(sql, &[])?
        } else {
            Vec::new()
        };
        let expected = scratch.query(sql, &[])?;
        if expected.len() > MAX_PREVIEW_INDEX_ROWS {
            return Err(TimelineError::Invalid("derived-index row limit exceeded"));
        }
        let actual_set: BTreeSet<_> = actual.iter().collect();
        let expected_set: BTreeSet<_> = expected.iter().collect();
        drift.push(DerivedIndexDrift {
            table,
            exists,
            expected_rows: expected.len(),
            actual_rows: actual.len(),
            missing_rows: expected_set.difference(&actual_set).count(),
            unexpected_rows: actual_set.difference(&expected_set).count(),
        });
        fingerprint_rows.push((table, exists, actual));
    }
    let bytes =
        serde_json::to_vec(&fingerprint_rows).map_err(|error| storage(error.to_string()))?;
    let confirmation = digest(&SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(DerivedIndexPreview {
        revision: revision(connection)?,
        canonical_events,
        drift,
        confirmation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::{
        identity::CharacterId,
        timeline::domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository},
    };
    use serde_json::json;
    use uuid::Uuid;

    struct TempDb(PathBuf);
    impl TempDb {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "neuromita-index-maintenance-{}.sqlite3",
                Uuid::new_v4()
            )))
        }
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
            }
        }
    }
    fn seeded() -> (TempDb, BranchId, crate::identity::EventId) {
        let db = TempDb::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut draft = EventDraft::new(EventKind::UnityEvent);
        draft.observers.insert(CharacterId::Crazy);
        let id = draft.id;
        repo.append(branch, 0, "canonical", json!({}), vec![draft])
            .unwrap();
        (db, branch, id)
    }

    #[test]
    fn read_only_preview_does_not_recreate_missing_table_and_confirmed_repair_restores_it() {
        let (db, branch, id) = seeded();
        let conn = Connection::open(&db.0).unwrap();
        let initial_revision = revision(&conn).unwrap();
        conn.exec("DROP TABLE timeline_fact_keys").unwrap();
        let before = SqliteTimelineRepository::inspect_derived_indexes(&db.0).unwrap();
        assert!(before.requires_repair());
        assert!(!before.drift[0].exists);
        assert_eq!(before.drift[0].missing_rows, 1);
        assert!(
            conn.query(
                "SELECT 1 FROM sqlite_master WHERE name='timeline_fact_keys'",
                &[]
            )
            .unwrap()
            .is_empty()
        );
        let after = SqliteTimelineRepository::rebuild_derived_indexes_confirmed(
            &db.0,
            &before.confirmation,
        )
        .unwrap();
        assert!(!after.requires_repair());
        assert_eq!(after.revision, initial_revision);
        let repo = SqliteTimelineRepository::open(&db.0).unwrap();
        assert_eq!(repo.event(branch, id).unwrap().draft.id, id);
        assert_eq!(
            SqliteTimelineRepository::inspect_derived_indexes(&db.0).unwrap(),
            after
        );
    }

    #[test]
    fn stale_confirmation_refuses_source_and_derived_mutations_without_writing() {
        let (db, branch, _) = seeded();
        let first = SqliteTimelineRepository::inspect_derived_indexes(&db.0).unwrap();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        repo.append(
            branch,
            1,
            "new",
            json!({}),
            vec![EventDraft::new(EventKind::UnityEvent)],
        )
        .unwrap();
        assert_eq!(
            SqliteTimelineRepository::rebuild_derived_indexes_confirmed(&db.0, &first.confirmation),
            Err(TimelineError::ConcurrentWrite)
        );
        let second = SqliteTimelineRepository::inspect_derived_indexes(&db.0).unwrap();
        assert_ne!(first.confirmation, second.confirmation);
        let conn = Connection::open(&db.0).unwrap();
        conn.exec("DELETE FROM timeline_observers").unwrap();
        assert_eq!(
            SqliteTimelineRepository::rebuild_derived_indexes_confirmed(
                &db.0,
                &second.confirmation
            ),
            Err(TimelineError::ConcurrentWrite)
        );
    }

    #[test]
    fn orphan_canonical_event_is_rejected_before_any_derived_writes() {
        let (db, branch, _) = seeded();
        let conn = Connection::open(&db.0).unwrap();
        let original_keys = conn
            .query("SELECT event_id FROM timeline_fact_keys", &[])
            .unwrap();
        conn.exec("PRAGMA foreign_keys = OFF").unwrap();
        conn.write(
            "INSERT INTO timeline_events(branch_id,sequence,fact_json) VALUES(?,?,?)",
            &[&uuid::Uuid::new_v4().to_string(), &"1", &"{}"],
        )
        .unwrap();
        assert!(matches!(
            SqliteTimelineRepository::inspect_derived_indexes(&db.0),
            Err(TimelineError::Storage(_))
        ));
        assert!(
            SqliteTimelineRepository::rebuild_derived_indexes_confirmed(&db.0, &"0".repeat(64))
                .is_err()
        );
        assert_eq!(
            conn.query("SELECT event_id FROM timeline_fact_keys", &[])
                .unwrap(),
            original_keys
        );
        assert_eq!(
            SqliteTimelineRepository::open(&db.0)
                .unwrap()
                .world(branch)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn invalid_source_and_missing_file_never_create_or_repair_data() {
        let (db, branch, _) = seeded();
        let conn = Connection::open(&db.0).unwrap();
        conn.write(
            "UPDATE timeline_events SET fact_json=? WHERE branch_id=? AND sequence=1",
            &[&"invalid-json", &branch.0.to_string()],
        )
        .unwrap();
        assert!(SqliteTimelineRepository::inspect_derived_indexes(&db.0).is_err());
        assert!(
            SqliteTimelineRepository::rebuild_derived_indexes_confirmed(&db.0, &"0".repeat(64))
                .is_err()
        );
        let missing = std::env::temp_dir().join(format!("missing-index-{}.db", Uuid::new_v4()));
        assert!(SqliteTimelineRepository::inspect_derived_indexes(&missing).is_err());
        assert!(
            SqliteTimelineRepository::rebuild_derived_indexes_confirmed(&missing, &"0".repeat(64))
                .is_err()
        );
        assert!(!missing.exists());
    }
}
