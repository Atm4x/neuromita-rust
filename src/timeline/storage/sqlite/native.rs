use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    path::Path,
    ptr::{self, NonNull},
};

use crate::timeline::domain::TimelineError;

use super::storage;

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
pub(super) struct Connection(NonNull<Sqlite3>);

impl Connection {
    pub(super) fn open(path: &Path) -> Result<Self, TimelineError> {
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

    pub(super) fn exec(&self, sql: &str) -> Result<(), TimelineError> {
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

    pub(super) fn query(
        &self,
        sql: &str,
        params: &[&str],
    ) -> Result<Vec<Vec<String>>, TimelineError> {
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

    pub(super) fn write(&self, sql: &str, params: &[&str]) -> Result<(), TimelineError> {
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
