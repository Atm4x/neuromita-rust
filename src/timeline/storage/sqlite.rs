use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    path::{Path, PathBuf},
    ptr::{self, NonNull},
};

use serde_json::Value;
use uuid::Uuid;

use crate::identity::{CharacterId, EventId};
use crate::timeline::domain::{
    Branch, BranchId, BranchInfo, EventDraft, KnownEvent, Receipt, SessionId, TimelineError,
    TimelineFact, TimelineRepository, TimelineStore,
};
use crate::timeline::selection::{TimelineSelection, TimelineSelectionRepository};

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}
#[repr(C)]
struct SqliteStatement {
    _private: [u8; 0],
}

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        db: *mut *mut Sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int,
        >,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        sql: *const c_char,
        size: c_int,
        stmt: *mut *mut SqliteStatement,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_finalize(stmt: *mut SqliteStatement) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut SqliteStatement,
        index: c_int,
        text: *const c_char,
        size: c_int,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut SqliteStatement) -> c_int;
    fn sqlite3_column_count(stmt: *mut SqliteStatement) -> c_int;
    fn sqlite3_column_text(stmt: *mut SqliteStatement, index: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut SqliteStatement, index: c_int) -> c_int;
    fn sqlite3_busy_timeout(db: *mut Sqlite3, ms: c_int) -> c_int;
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_READWRITE_CREATE: c_int = 0x0000_0006;
const SQLITE_FULLMUTEX: c_int = 0x0001_0000;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS timeline_schema(version INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS timeline_revision(
    slot INTEGER PRIMARY KEY CHECK(slot=1),
    revision INTEGER NOT NULL CHECK(revision>=0)
);
INSERT OR IGNORE INTO timeline_revision(slot,revision) VALUES(1,0);
CREATE TABLE IF NOT EXISTS timeline_branches(
    branch_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    parent_id TEXT,
    fork_sequence INTEGER NOT NULL,
    head_sequence INTEGER NOT NULL,
    checkpoint_id TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS timeline_session_roots
    ON timeline_branches(session_id) WHERE parent_id IS NULL;
CREATE TABLE IF NOT EXISTS timeline_events(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    fact_json TEXT NOT NULL,
    PRIMARY KEY(branch_id, sequence),
    FOREIGN KEY(branch_id) REFERENCES timeline_branches(branch_id)
);
CREATE TABLE IF NOT EXISTS timeline_observers(
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    character_id TEXT NOT NULL,
    PRIMARY KEY(branch_id, sequence, character_id),
    FOREIGN KEY(branch_id,sequence) REFERENCES timeline_events(branch_id,sequence) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS timeline_observers_by_character
    ON timeline_observers(character_id,branch_id,sequence);
CREATE TABLE IF NOT EXISTS timeline_receipts(
    branch_id TEXT NOT NULL,
    operation_key TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY(branch_id, operation_key),
    FOREIGN KEY(branch_id) REFERENCES timeline_branches(branch_id)
);
CREATE TABLE IF NOT EXISTS timeline_active_selection(
    slot TEXT PRIMARY KEY CHECK(slot='core'),
    branch_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    FOREIGN KEY(branch_id) REFERENCES timeline_branches(branch_id)
);
";

struct Connection(NonNull<Sqlite3>);

impl Connection {
    fn open(path: &Path) -> Result<Self, TimelineError> {
        let path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| storage("database path contains NUL"))?;
        let mut raw = ptr::null_mut();
        let result = unsafe {
            sqlite3_open_v2(
                path.as_ptr(),
                &mut raw,
                SQLITE_READWRITE_CREATE | SQLITE_FULLMUTEX,
                ptr::null(),
            )
        };
        let Some(raw) = NonNull::new(raw) else {
            return Err(storage("SQLite could not allocate a connection"));
        };
        let connection = Self(raw);
        if result != SQLITE_OK {
            return Err(connection.error());
        }
        if unsafe { sqlite3_busy_timeout(connection.0.as_ptr(), 10_000) } != SQLITE_OK {
            return Err(connection.error());
        }
        connection.exec("PRAGMA foreign_keys = ON")?;
        Ok(connection)
    }

    fn error(&self) -> TimelineError {
        let message = unsafe { CStr::from_ptr(sqlite3_errmsg(self.0.as_ptr())) }
            .to_string_lossy()
            .into_owned();
        storage(message)
    }

    fn exec(&self, sql: &str) -> Result<(), TimelineError> {
        let sql = CString::new(sql).map_err(|_| storage("invalid SQL"))?;
        if unsafe {
            sqlite3_exec(
                self.0.as_ptr(),
                sql.as_ptr(),
                None,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } != SQLITE_OK
        {
            return Err(self.error());
        }
        Ok(())
    }

    fn query(&self, sql: &str, params: &[&str]) -> Result<Vec<Vec<String>>, TimelineError> {
        let sql = CString::new(sql).map_err(|_| storage("invalid SQL"))?;
        let mut raw = ptr::null_mut();
        if unsafe {
            sqlite3_prepare_v2(self.0.as_ptr(), sql.as_ptr(), -1, &mut raw, ptr::null_mut())
        } != SQLITE_OK
        {
            return Err(self.error());
        }
        let stmt = Statement {
            ptr: NonNull::new(raw).ok_or_else(|| storage("empty SQL statement"))?,
            connection: self,
        };
        let arguments: Vec<_> = params
            .iter()
            .map(|param| CString::new(*param).map_err(|_| storage("SQL parameter contains NUL")))
            .collect::<Result<_, _>>()?;
        for (index, argument) in arguments.iter().enumerate() {
            let len = c_int::try_from(argument.as_bytes().len())
                .map_err(|_| storage("SQL parameter too large"))?;
            let result = unsafe {
                sqlite3_bind_text(
                    stmt.ptr.as_ptr(),
                    (index + 1) as c_int,
                    argument.as_ptr(),
                    len,
                    None,
                )
            };
            if result != SQLITE_OK {
                return Err(self.error());
            }
        }
        let mut rows = Vec::new();
        loop {
            let result = unsafe { sqlite3_step(stmt.ptr.as_ptr()) };
            if result == SQLITE_DONE {
                break;
            }
            if result != SQLITE_ROW {
                return Err(self.error());
            }
            let fields = unsafe { sqlite3_column_count(stmt.ptr.as_ptr()) };
            let mut row = Vec::with_capacity(fields as usize);
            for index in 0..fields {
                let ptr = unsafe { sqlite3_column_text(stmt.ptr.as_ptr(), index) };
                if ptr.is_null() {
                    row.push(String::new());
                    continue;
                }
                let size = unsafe { sqlite3_column_bytes(stmt.ptr.as_ptr(), index) };
                let bytes = unsafe { std::slice::from_raw_parts(ptr, size as usize) };
                let string = String::from_utf8(bytes.to_vec())
                    .map_err(|_| storage("invalid SQLite UTF-8 value"))?;
                row.push(string);
            }
            rows.push(row);
        }
        Ok(rows)
    }

    fn write(&self, sql: &str, params: &[&str]) -> Result<(), TimelineError> {
        if !self.query(sql, params)?.is_empty() {
            return Err(storage("unexpected SQL results"));
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            sqlite3_close(self.0.as_ptr());
        }
    }
}

struct Statement<'a> {
    ptr: NonNull<SqliteStatement>,
    connection: &'a Connection,
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        let result = unsafe { sqlite3_finalize(self.ptr.as_ptr()) };
        if result != SQLITE_OK {
            let _ = self.connection.error();
        }
    }
}

fn storage(message: impl Into<String>) -> TimelineError {
    TimelineError::Storage(message.into())
}

fn parse_uuid(text: &str) -> Result<Uuid, TimelineError> {
    Uuid::parse_str(text).map_err(|_| storage("invalid persisted UUID"))
}
fn parse_u64(text: &str) -> Result<u64, TimelineError> {
    text.parse()
        .map_err(|_| storage("invalid persisted sequence"))
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, TimelineError> {
    serde_json::from_str(text).map_err(|error| storage(error.to_string()))
}
fn encode<T: serde::Serialize>(value: &T) -> Result<String, TimelineError> {
    serde_json::to_string(value).map_err(|error| storage(error.to_string()))
}

/// SQLite is a replaceable transaction owner, not a second implementation of timeline rules.
/// SQLite 3 development libraries are required when the `sqlite-system` feature is enabled.
pub struct SqliteTimelineRepository {
    path: PathBuf,
    cache: Option<(BranchId, u64, TimelineStore)>,
}

impl SqliteTimelineRepository {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TimelineError> {
        let path = path.as_ref();
        if path.to_string_lossy() == ":memory:" {
            return Err(storage("SQLite timeline requires a file path"));
        }
        let repo = Self {
            path: path.to_path_buf(),
            cache: None,
        };
        let conn = repo.connection()?;
        conn.exec("PRAGMA journal_mode = WAL")?;
        conn.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            conn.exec(SCHEMA)?;
            let versions = conn.query("SELECT version FROM timeline_schema", &[])?;
            match versions.as_slice() {
                [] => conn.write("INSERT INTO timeline_schema(version) VALUES('1')", &[]),
                [version] if version[0] == "1" => Ok(()),
                _ => Err(storage("unsupported timeline schema version")),
            }
        })();
        if result.is_ok() {
            conn.exec("COMMIT")?;
        } else {
            conn.exec("ROLLBACK")?;
        }
        result?;
        Ok(repo)
    }

    fn connection(&self) -> Result<Connection, TimelineError> {
        Connection::open(&self.path)
    }

    fn with_write<T>(
        &mut self,
        apply: impl FnOnce(&Connection, &mut TimelineStore) -> Result<T, TimelineError>,
    ) -> Result<T, TimelineError> {
        self.cache = None;
        let connection = self.connection()?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let mut store = load(&connection)?;
            let value = apply(&connection, &mut store)?;
            advance_revision(&connection, revision(&connection)?)?;
            Ok(value)
        })();
        if result.is_ok() {
            connection.exec("COMMIT")?;
        } else {
            connection.exec("ROLLBACK")?;
        }
        result
    }
}

fn revision(connection: &Connection) -> Result<u64, TimelineError> {
    let rows = connection.query("SELECT revision FROM timeline_revision WHERE slot=1", &[])?;
    let [row] = rows.as_slice() else {
        return Err(storage("timeline revision metadata missing"));
    };
    parse_u64(&row[0])
}

fn advance_revision(connection: &Connection, current: u64) -> Result<u64, TimelineError> {
    let next = current
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or(TimelineError::Invalid("timeline revision overflow"))?;
    connection.write(
        "UPDATE timeline_revision SET revision=? WHERE slot=1 AND revision=?",
        &[&next.to_string(), &current.to_string()],
    )?;
    if revision(connection)? != next {
        return Err(TimelineError::ConcurrentWrite);
    }
    Ok(next)
}

fn branch_info(connection: &Connection, branch: BranchId) -> Result<BranchInfo, TimelineError> {
    let rows = connection.query(
        "SELECT session_id,ifnull(parent_id,''),fork_sequence,head_sequence,ifnull(checkpoint_id,'') FROM timeline_branches WHERE branch_id=?",
        &[&branch.0.to_string()],
    )?;
    let [row] = rows.as_slice() else {
        return Err(TimelineError::NotFound);
    };
    Ok(BranchInfo {
        id: branch,
        session_id: SessionId(parse_uuid(&row[0])?),
        parent: if row[1].is_empty() {
            None
        } else {
            Some(BranchId(parse_uuid(&row[1])?))
        },
        fork_sequence: parse_u64(&row[2])?,
        head_sequence: parse_u64(&row[3])?,
        checkpoint: if row[4].is_empty() {
            None
        } else {
            Some(EventId(parse_uuid(&row[4])?))
        },
    })
}

fn load(connection: &Connection) -> Result<TimelineStore, TimelineError> {
    let mut branches = Vec::new();
    for row in connection.query("SELECT branch_id, session_id, ifnull(parent_id,''), fork_sequence, head_sequence, ifnull(checkpoint_id,'') FROM timeline_branches ORDER BY branch_id", &[])? {
        let id = BranchId(parse_uuid(&row[0])?);
        let info = BranchInfo {
            id,
            session_id: SessionId(parse_uuid(&row[1])?),
            parent: if row[2].is_empty() { None } else { Some(BranchId(parse_uuid(&row[2])?)) },
            fork_sequence: parse_u64(&row[3])?,
            head_sequence: parse_u64(&row[4])?,
            checkpoint: if row[5].is_empty() { None } else { Some(EventId(parse_uuid(&row[5])?)) },
        };
        let mut events = Vec::new();
        for event in connection.query("SELECT fact_json FROM timeline_events WHERE branch_id=? ORDER BY sequence", &[&row[0]])? {
            events.push(decode(&event[0])?);
        }
        let mut receipts = std::collections::HashMap::new();
        for receipt in connection.query("SELECT operation_key, receipt_json FROM timeline_receipts WHERE branch_id=?", &[&row[0]])? {
            receipts.insert(receipt[0].clone(), decode::<Receipt>(&receipt[1])?);
        }
        branches.push(Branch { info, events, receipts });
    }
    TimelineStore::load_persisted(branches)
}

fn load_lineage(connection: &Connection, leaf: BranchId) -> Result<TimelineStore, TimelineError> {
    let mut infos = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut next = Some(leaf);
    while let Some(id) = next {
        if !visited.insert(id) {
            return Err(storage("cyclic branch lineage"));
        }
        let info = branch_info(connection, id)?;
        next = info.parent;
        infos.push(info);
    }
    let mut branches = Vec::with_capacity(infos.len());
    for info in infos.into_iter().rev() {
        let branch_id = info.id.0.to_string();
        let mut events = Vec::new();
        for row in connection.query(
            "SELECT fact_json FROM timeline_events WHERE branch_id=? ORDER BY sequence",
            &[&branch_id],
        )? {
            events.push(decode(&row[0])?);
        }
        let mut receipts = std::collections::HashMap::new();
        if info.id == leaf {
            for row in connection.query(
                "SELECT operation_key, receipt_json FROM timeline_receipts WHERE branch_id=?",
                &[&branch_id],
            )? {
                receipts.insert(row[0].clone(), decode::<Receipt>(&row[1])?);
            }
        }
        branches.push(Branch {
            info,
            events,
            receipts,
        });
    }
    TimelineStore::load_persisted(branches)
}

fn save_branch(
    connection: &Connection,
    store: &TimelineStore,
    branch_id: BranchId,
) -> Result<(), TimelineError> {
    let branch = store
        .persisted_branches()
        .into_iter()
        .find(|branch| branch.info.id == branch_id)
        .ok_or(TimelineError::NotFound)?;
    let info = branch.info;
    let branch_id = info.id.0.to_string();
    let session_id = info.session_id.0.to_string();
    let parent = info.parent.map(|id| id.0.to_string()).unwrap_or_default();
    let fork = info.fork_sequence.to_string();
    let head = info.head_sequence.to_string();
    let checkpoint = info
        .checkpoint
        .map(|id| id.0.to_string())
        .unwrap_or_default();
    connection.write("INSERT INTO timeline_branches(branch_id,session_id,parent_id,fork_sequence,head_sequence,checkpoint_id) VALUES (?, ?, nullif(?,''), ?, ?, nullif(?,'')) ON CONFLICT(branch_id) DO UPDATE SET head_sequence=excluded.head_sequence", &[&branch_id, &session_id, &parent, &fork, &head, &checkpoint])?;
    connection.write(
        "DELETE FROM timeline_events WHERE branch_id=?",
        &[&branch_id],
    )?;
    for fact in &branch.events {
        connection.write(
            "INSERT INTO timeline_events(branch_id,sequence,fact_json) VALUES(?, ?, ?)",
            &[&branch_id, &fact.sequence.to_string(), &encode(fact)?],
        )?;
        for observer in &fact.draft.observers {
            connection.write(
                "INSERT INTO timeline_observers(branch_id,sequence,character_id) VALUES(?, ?, ?)",
                &[
                    &branch_id,
                    &fact.sequence.to_string(),
                    &observer.to_string(),
                ],
            )?;
        }
    }
    connection.write(
        "DELETE FROM timeline_receipts WHERE branch_id=?",
        &[&branch_id],
    )?;
    for (key, receipt) in &branch.receipts {
        connection.write(
            "INSERT INTO timeline_receipts(branch_id,operation_key,receipt_json) VALUES(?, ?, ?)",
            &[&branch_id, key, &encode(receipt)?],
        )?;
    }
    Ok(())
}

fn save_append(
    connection: &Connection,
    store: &TimelineStore,
    branch_id: BranchId,
    previous_head: u64,
    key: &str,
    events: &[TimelineFact],
) -> Result<(), TimelineError> {
    let branch = branch_id.0.to_string();
    let new_head = store.branch(branch_id)?.head_sequence.to_string();
    let previous = previous_head.to_string();
    connection.write(
        "UPDATE timeline_branches SET head_sequence=? WHERE branch_id=? AND head_sequence=?",
        &[&new_head, &branch, &previous],
    )?;
    for event in events {
        connection.write(
            "INSERT INTO timeline_events(branch_id,sequence,fact_json) VALUES(?, ?, ?)",
            &[&branch, &event.sequence.to_string(), &encode(event)?],
        )?;
        for observer in &event.draft.observers {
            connection.write(
                "INSERT INTO timeline_observers(branch_id,sequence,character_id) VALUES(?, ?, ?)",
                &[&branch, &event.sequence.to_string(), &observer.to_string()],
            )?;
        }
    }
    let receipt = store
        .persisted_receipt(branch_id, key)
        .ok_or_else(|| storage("missing committed operation receipt"))?;
    connection.write(
        "INSERT INTO timeline_receipts(branch_id,operation_key,receipt_json) VALUES(?, ?, ?)",
        &[&branch, key, &encode(receipt)?],
    )?;
    Ok(())
}

fn read_page(
    conn: &Connection,
    branch: BranchId,
    after: u64,
    through: Option<u64>,
    limit: usize,
    observer: Option<CharacterId>,
) -> Result<Vec<TimelineFact>, TimelineError> {
    if limit == 0 {
        return Err(TimelineError::Invalid("read limit must be positive"));
    }
    let mut branches = Vec::new();
    let mut next = Some(branch);
    let mut ceiling = through;
    let mut visited = std::collections::HashSet::new();
    while let Some(id) = next {
        if !visited.insert(id) {
            return Err(storage("cyclic branch lineage"));
        }
        let rows = conn.query(
            "SELECT ifnull(parent_id,''),fork_sequence,head_sequence FROM timeline_branches WHERE branch_id=?",
            &[&id.0.to_string()],
        )?;
        let [row] = rows.as_slice() else {
            return Err(TimelineError::NotFound);
        };
        let fork = parse_u64(&row[1])?;
        let head = parse_u64(&row[2])?;
        let current = ceiling.unwrap_or(head);
        if id == branch && current > head {
            return Err(TimelineError::Invalid("read position exceeds branch head"));
        }
        branches.push((id, fork, current.min(head)));
        ceiling = Some(current.min(fork));
        next = if row[0].is_empty() {
            None
        } else {
            Some(BranchId(parse_uuid(&row[0])?))
        };
    }
    let mut results = Vec::new();
    for (id, fork, ceiling) in branches.into_iter().rev() {
        let remaining = limit - results.len();
        if remaining == 0 {
            break;
        }
        let id = id.0.to_string();
        let lower = fork.max(after).to_string();
        let ceiling = ceiling.to_string();
        let limit = remaining.to_string();
        let rows = if let Some(observer) = observer {
            conn.query(
                "SELECT e.fact_json FROM timeline_events e WHERE e.branch_id=? AND e.sequence>? AND e.sequence<=? AND EXISTS(SELECT 1 FROM timeline_observers o WHERE o.branch_id=e.branch_id AND o.sequence=e.sequence AND o.character_id=?) ORDER BY e.sequence LIMIT ?",
                &[&id, &lower, &ceiling, &observer.to_string(), &limit],
            )?
        } else {
            conn.query(
                "SELECT fact_json FROM timeline_events WHERE branch_id=? AND sequence>? AND sequence<=? ORDER BY sequence LIMIT ?",
                &[&id, &lower, &ceiling, &limit],
            )?
        };
        for row in rows {
            results.push(decode(&row[0])?);
        }
    }
    Ok(results)
}

impl TimelineRepository for SqliteTimelineRepository {
    fn create_session(
        &mut self,
        session: SessionId,
        root: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        self.with_write(|connection, store| {
            let already_exists = store.branch(root).is_ok();
            let info = store.create_session(session, root)?;
            if !already_exists {
                save_branch(connection, store, root)?;
            }
            Ok(info)
        })
    }
    fn branch(&self, id: BranchId) -> Result<BranchInfo, TimelineError> {
        branch_info(&self.connection()?, id)
    }
    fn world(&self, id: BranchId) -> Result<Vec<TimelineFact>, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN")?;
        let result = read_page(&connection, id, 0, None, i64::MAX as usize, None);
        connection.exec("ROLLBACK")?;
        result
    }
    fn event(&self, branch: BranchId, id: EventId) -> Result<TimelineFact, TimelineError> {
        self.world(branch)?
            .into_iter()
            .find(|fact| fact.draft.id == id)
            .ok_or(TimelineError::NotFound)
    }
    fn read(
        &self,
        branch: BranchId,
        after: u64,
        through: Option<u64>,
        limit: usize,
        observer: Option<CharacterId>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        let conn = self.connection()?;
        conn.exec("BEGIN")?;
        let result = read_page(&conn, branch, after, through, limit, observer);
        conn.exec("ROLLBACK")?;
        result
    }
    fn append(
        &mut self,
        branch: BranchId,
        expected_head: u64,
        key: &str,
        command: Value,
        drafts: Vec<EventDraft>,
    ) -> Result<Vec<TimelineFact>, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            let current_revision = revision(&connection)?;
            let mut store = match self.cache.take() {
                Some((cached_branch, cached_revision, cached))
                    if cached_branch == branch && cached_revision == current_revision =>
                {
                    cached
                }
                _ => load_lineage(&connection, branch)?,
            };
            let previous_head = store.branch(branch)?.head_sequence;
            let already_committed = store.persisted_receipt(branch, key).is_some();
            let facts = store.append(branch, expected_head, key, command, drafts)?;
            let new_revision = if already_committed {
                current_revision
            } else {
                save_append(&connection, &store, branch, previous_head, key, &facts)?;
                advance_revision(&connection, current_revision)?
            };
            Ok((facts, new_revision, store))
        })();
        match result {
            Ok((facts, cached_revision, store)) => {
                if let Err(error) = connection.exec("COMMIT") {
                    self.cache = None;
                    return Err(error);
                }
                self.cache = Some((branch, cached_revision, store));
                Ok(facts)
            }
            Err(error) => {
                self.cache = None;
                connection.exec("ROLLBACK")?;
                Err(error)
            }
        }
    }
    fn fork(
        &mut self,
        parent: BranchId,
        checkpoint: EventId,
        child: BranchId,
    ) -> Result<BranchInfo, TimelineError> {
        self.with_write(|connection, store| {
            let already_exists = store.branch(child).is_ok();
            let info = store.fork(parent, checkpoint, child)?;
            if !already_exists {
                save_branch(connection, store, child)?;
            }
            Ok(info)
        })
    }
    fn view(
        &self,
        branch: BranchId,
        character: CharacterId,
        through: Option<u64>,
    ) -> Result<Vec<KnownEvent>, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN")?;
        let result = read_page(&connection, branch, 0, through, i64::MAX as usize, None)
            .map(|facts| crate::timeline::domain::character_view(&facts, character));
        connection.exec("ROLLBACK")?;
        result
    }
    fn knows(
        &self,
        branch: BranchId,
        character: CharacterId,
        event: EventId,
        through: Option<u64>,
    ) -> Result<bool, TimelineError> {
        Ok(self
            .view(branch, character, through)?
            .iter()
            .any(|known| known.fact.draft.id == event))
    }
}

impl TimelineSelectionRepository for SqliteTimelineRepository {
    fn selection(&self) -> Result<Option<TimelineSelection>, TimelineError> {
        let connection = self.connection()?;
        let result = connection.query(
            "SELECT branch_id, generation FROM timeline_active_selection WHERE slot='core'",
            &[],
        )?;
        match result.as_slice() {
            [] => Ok(None),
            [row] => Ok(Some(TimelineSelection {
                branch_id: BranchId(parse_uuid(&row[0])?),
                generation: parse_u64(&row[1])?,
            })),
            _ => Err(storage("duplicate active session selection")),
        }
    }

    fn select_branch(
        &mut self,
        branch: BranchId,
        expected: Option<TimelineSelection>,
    ) -> Result<TimelineSelection, TimelineError> {
        let connection = self.connection()?;
        connection.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            branch_info(&connection, branch)?;
            let rows = connection.query(
                "SELECT branch_id, generation FROM timeline_active_selection WHERE slot='core'",
                &[],
            )?;
            let current = match rows.as_slice() {
                [] => None,
                [row] => Some(TimelineSelection {
                    branch_id: BranchId(parse_uuid(&row[0])?),
                    generation: parse_u64(&row[1])?,
                }),
                _ => return Err(storage("duplicate active selection")),
            };
            if current != expected {
                return Err(TimelineError::ConcurrentWrite);
            }
            let generation = current
                .map_or(Some(1), |value| value.generation.checked_add(1))
                .filter(|value| *value <= i64::MAX as u64)
                .ok_or(TimelineError::Invalid("selection generation overflow"))?;
            connection.write(
                "INSERT INTO timeline_active_selection(slot,branch_id,generation) VALUES('core', ?, ?) ON CONFLICT(slot) DO UPDATE SET branch_id=excluded.branch_id,generation=excluded.generation",
                &[&branch.0.to_string(), &generation.to_string()],
            )?;
            Ok(TimelineSelection {
                branch_id: branch,
                generation,
            })
        })();
        if result.is_ok() {
            connection.exec("COMMIT")?;
        } else {
            connection.exec("ROLLBACK")?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestDb(PathBuf);
    impl TestDb {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("neuromita-sqlite-failure-{}.db", Uuid::new_v4())),
            )
        }
    }
    impl Drop for TestDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
            }
        }
    }

    #[test]
    fn warmed_cache_is_discarded_after_sqlite_failure() {
        let db = TestDb::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        repo.append(
            branch,
            0,
            "first",
            json!({}),
            vec![EventDraft::new(
                crate::timeline::domain::EventKind::UnityEvent,
            )],
        )
        .unwrap();
        assert!(repo.cache.is_some());
        let conn = Connection::open(&db.0).unwrap();
        conn.exec("CREATE TRIGGER timeline_test_warm_fail BEFORE INSERT ON timeline_events BEGIN SELECT RAISE(ABORT, 'injected warmed failure'); END").unwrap();
        assert!(matches!(
            repo.append(
                branch,
                1,
                "retry",
                json!({}),
                vec![EventDraft::new(
                    crate::timeline::domain::EventKind::UnityEvent
                )]
            ),
            Err(TimelineError::Storage(_))
        ));
        assert!(repo.cache.is_none());
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);
        conn.exec("DROP TRIGGER timeline_test_warm_fail").unwrap();
        repo.append(
            branch,
            1,
            "retry",
            json!({}),
            vec![EventDraft::new(
                crate::timeline::domain::EventKind::UnityEvent,
            )],
        )
        .unwrap();
        assert_eq!(repo.world(branch).unwrap().len(), 2);
    }

    #[test]
    fn sqlite_failure_after_head_update_rolls_back_everything() {
        let db = TestDb::new();
        let mut repo = SqliteTimelineRepository::open(&db.0).unwrap();
        let branch = BranchId::new();
        repo.create_session(SessionId::new(), branch).unwrap();
        let conn = Connection::open(&db.0).unwrap();
        conn.exec("CREATE TRIGGER timeline_test_fail BEFORE INSERT ON timeline_events BEGIN SELECT RAISE(ABORT, 'injected failure'); END").unwrap();
        let draft = EventDraft::new(crate::timeline::domain::EventKind::UnityEvent);
        assert!(matches!(
            repo.append(branch, 0, "operation", json!({}), vec![draft.clone()]),
            Err(TimelineError::Storage(_))
        ));
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 0);
        assert!(repo.world(branch).unwrap().is_empty());
        conn.exec("DROP TRIGGER timeline_test_fail").unwrap();
        assert!(
            repo.append(branch, 0, "operation", json!({}), vec![draft])
                .is_ok()
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 1);
    }
}
