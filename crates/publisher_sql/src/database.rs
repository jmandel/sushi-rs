use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;

#[cfg(not(target_arch = "wasm32"))]
use rusqlite::ffi;
use serde::Serialize;
use serde_json::{Map, Number, Value};
#[cfg(target_arch = "wasm32")]
use sqlite_wasm_rs as ffi;
use thiserror::Error;

use crate::snapshot::QuerySnapshot;

const MAX_QUERY_BYTES: usize = 256 * 1024;
const MAX_ROWS: usize = 10_000;
const MAX_RESULT_CELLS: usize = 1_000_000;
pub(crate) const MAX_RENDERED_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESULT_BYTES: usize = MAX_RENDERED_BYTES;
const PROGRESS_INTERVAL: c_int = 1_000;
const MAX_PROGRESS_CALLBACKS: u32 = 20_000;

const SCHEMA: &str = r#"
CREATE TABLE Metadata (
  Key integer NOT NULL, Name nvarchar NOT NULL, Value nvarchar NOT NULL,
  PRIMARY KEY (Key));
CREATE TABLE Resources (
  Key integer NOT NULL, Type nvarchar NOT NULL, Custom integer NOT NULL,
  Id nvarchar NOT NULL, Web nvarchar NOT NULL, Url nvarchar NULL,
  Version nvarchar NULL, Status nvarchar NULL, Date nvarchar NULL,
  Name nvarchar NULL, Title nvarchar NULL, Experimental nvarchar NULL,
  Realm nvarchar NULL, Description nvarchar NULL, Purpose nvarchar NULL,
  Copyright nvarchar NULL, CopyrightLabel nvarchar NULL,
  derivation nvarchar NULL, standardStatus nvarchar NULL, kind nvarchar NULL,
  sdType nvarchar NULL, base nvarchar NULL, content nvarchar NULL,
  supplements nvarchar NULL, Json nvarchar NOT NULL, PRIMARY KEY (Key));
CREATE TABLE Properties (
  Key integer NOT NULL, ResourceKey integer NOT NULL, Code varchar NOT NULL,
  Uri varchar NULL, Description varchar NULL, Type varchar NULL,
  PRIMARY KEY (Key));
CREATE TABLE Concepts (
  Key integer NOT NULL, ResourceKey integer NOT NULL, ParentKey integer NULL,
  Code varchar NULL, Display varchar NULL, Definition varchar NULL,
  PRIMARY KEY (Key));
CREATE TABLE ConceptProperties (
  Key integer NOT NULL, ResourceKey integer NOT NULL, ConceptKey integer NOT NULL,
  PropertyKey integer NULL, Code varchar NULL, Value varchar NULL,
  PRIMARY KEY (Key));
CREATE TABLE Designations (
  Key integer NOT NULL, ResourceKey integer NOT NULL, ConceptKey integer NOT NULL,
  UseSystem varchar NULL, UseCode varchar NULL, Lang varchar NULL,
  Value text NULL, PRIMARY KEY (Key));
CREATE TABLE ConceptMappings (
  Key integer NOT NULL, ResourceKey integer NOT NULL, SourceSystem varchar NULL,
  SourceVersion varchar NULL, SourceCode varchar NULL, Relationship varchar NULL,
  TargetSystem varchar NULL, TargetVersion varchar NULL, TargetCode varchar NULL,
  PRIMARY KEY (Key));
CREATE TABLE ValueSet_Codes (
  Key integer NOT NULL, ResourceKey integer NOT NULL,
  ValueSetUri nvarchar NOT NULL, ValueSetVersion nvarchar NOT NULL,
  System nvarchar NOT NULL, Version nvarchar NULL, Code nvarchar NOT NULL,
  Display nvarchar NULL, PRIMARY KEY (Key));
CREATE TABLE CodeSystemList (
  CodeSystemListKey integer NOT NULL, ViewType integer NOT NULL,
  ResourceKey integer NULL, Url nvarchar NULL, Version nvarchar NULL,
  Status nvarchar NULL, Name nvarchar NULL, Title nvarchar NULL,
  Description nvarchar NULL, PRIMARY KEY (CodeSystemListKey));
CREATE TABLE CodeSystemListOIDs (
  CodeSystemListKey integer NOT NULL, OID nvarchar NOT NULL,
  PRIMARY KEY (CodeSystemListKey, OID));
CREATE TABLE CodeSystemListRefs (
  CodeSystemListKey integer NOT NULL, Type nvarchar NOT NULL,
  Id nvarchar NOT NULL, ResourceKey integer NULL, Title nvarchar NULL,
  Web nvarchar NULL, PRIMARY KEY (CodeSystemListKey, Type, Id));
CREATE TABLE ValueSetList (
  ValueSetListKey integer NOT NULL, ViewType integer NOT NULL,
  ResourceKey integer NULL, Url nvarchar NULL, Version nvarchar NULL,
  Status nvarchar NULL, Name nvarchar NULL, Title nvarchar NULL,
  Description nvarchar NULL, PRIMARY KEY (ValueSetListKey));
CREATE TABLE ValueSetListOIDs (
  ValueSetListKey integer NOT NULL, OID nvarchar NOT NULL,
  PRIMARY KEY (ValueSetListKey, OID));
CREATE TABLE ValueSetListSystems (
  ValueSetListKey integer NOT NULL, URL nvarchar NOT NULL,
  PRIMARY KEY (ValueSetListKey, URL));
CREATE TABLE ValueSetListSources (
  ValueSetListKey integer NOT NULL, Source nvarchar NOT NULL,
  PRIMARY KEY (ValueSetListKey, Source));
CREATE TABLE ValueSetListRefs (
  ValueSetListKey integer NOT NULL, Type nvarchar NOT NULL,
  Id nvarchar NOT NULL, ResourceKey integer NULL, Title nvarchar NULL,
  Web nvarchar NULL, PRIMARY KEY (ValueSetListKey, Type, Id));
"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub(crate) enum SqlValue {
    Null,
    Integer(i64),
    /// Keep SQLite's stable textual spelling so native/WASM normalization agrees.
    Real(String),
    Text(String),
    Blob(Vec<u8>),
}

impl SqlValue {
    pub(crate) fn display_text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Integer(value) => Some(value.to_string()),
            Self::Real(value) | Self::Text(value) => Some(value.clone()),
            Self::Blob(value) => Some(String::from_utf8_lossy(value).into_owned()),
        }
    }

    fn byte_len(&self) -> usize {
        match self {
            Self::Null => 0,
            Self::Integer(_) => std::mem::size_of::<i64>(),
            Self::Real(value) | Self::Text(value) => value.len(),
            Self::Blob(value) => value.len(),
        }
    }

    fn to_data_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(value) => Value::Number(Number::from(*value)),
            Self::Real(value) | Self::Text(value) => Value::String(value.clone()),
            // Use exact UTF-8 replacement decoding so native and WASM agree.
            Self::Blob(value) => Value::String(String::from_utf8_lossy(value).into_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
}

/// Shared per-page statement budget. Every statement is charged immediately
/// before SQLite prepares it, including the implicit CodeSystem checks used by
/// JSON SQL controls. Failed statements therefore still consume their work.
pub(crate) struct SqlStatementBudget {
    limit: usize,
    remaining: Cell<usize>,
}

impl SqlStatementBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            remaining: Cell::new(limit),
        }
    }

    fn charge(&self) -> Result<(), SqlError> {
        let remaining = self.remaining.get();
        if remaining == 0 {
            return Err(SqlError::StatementBudget(self.limit));
        }
        self.remaining.set(remaining - 1);
        Ok(())
    }
}

impl QueryResult {
    pub(crate) fn to_data(&self) -> Result<Value, SqlError> {
        let mut output = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut object = Map::new();
            for (name, value) in self.columns.iter().zip(row) {
                if object.insert(name.clone(), value.to_data_json()).is_some() {
                    return Err(SqlError::DuplicateColumn(name.clone()));
                }
            }
            output.push(Value::Object(object));
        }
        Ok(Value::Array(output))
    }
}

#[derive(Debug, Error)]
pub(crate) enum SqlError {
    #[error("SQL query is empty")]
    EmptyQuery,
    #[error("SQL query is larger than {MAX_QUERY_BYTES} bytes")]
    QueryTooLarge,
    #[error("SQL query contains an interior NUL byte")]
    InteriorNul,
    #[error("SQL query contains more than one statement")]
    MultipleStatements,
    #[error("SQL statement is not read-only")]
    NotReadOnly,
    #[error("SQL query exceeded the deterministic execution budget")]
    ExecutionBudget,
    #[error("page contains more than {0} SQL queries")]
    StatementBudget(usize),
    #[error("SQL query returned more than {MAX_ROWS} rows")]
    RowLimit,
    #[error("SQL query returned more than {MAX_RESULT_CELLS} cells")]
    CellLimit,
    #[error("SQL query returned more than {MAX_RESULT_BYTES} bytes")]
    ResultLimit,
    #[error("SQL query repeats result column {0}")]
    DuplicateColumn(String),
    #[error("SQL runtime initialization failed: {0}")]
    Initialization(String),
    #[error("SQLite error: {0}")]
    Sqlite(String),
}

/// One immutable in-memory SQL runtime. Initialization may be retained as a
/// typed failure so Publisher pages see an explicit inline error instead of
/// aborting an unrelated site build.
pub struct SqlRuntime {
    database: Result<Database, String>,
}

impl SqlRuntime {
    pub fn from_resources<'a>(resources: impl IntoIterator<Item = &'a Value>) -> Self {
        match QuerySnapshot::from_resources(resources) {
            Ok(snapshot) => Self::from_snapshot(snapshot),
            Err(error) => {
                let message = error.to_string();
                Self {
                    database: Err(message),
                }
            }
        }
    }

    fn from_snapshot(snapshot: QuerySnapshot) -> Self {
        let database = Database::from_snapshot(&snapshot).map_err(|error| error.to_string());
        Self { database }
    }

    #[cfg(test)]
    pub(crate) fn query(&self, query: &str) -> Result<QueryResult, SqlError> {
        self.query_with_budget(query, &SqlStatementBudget::new(usize::MAX))
    }

    pub(crate) fn query_with_budget(
        &self,
        query: &str,
        budget: &SqlStatementBudget,
    ) -> Result<QueryResult, SqlError> {
        budget.charge()?;
        let database = self
            .database
            .as_ref()
            .map_err(|error| SqlError::Initialization(error.clone()))?;
        database.query(&normalize_query(query)?)
    }

    #[cfg(test)]
    pub(crate) fn to_data(&self, query: &str) -> Result<Value, SqlError> {
        self.to_data_with_budget(query, &SqlStatementBudget::new(usize::MAX))
    }

    pub(crate) fn to_data_with_budget(
        &self,
        query: &str,
        budget: &SqlStatementBudget,
    ) -> Result<Value, SqlError> {
        let normalized_query = normalize_query(query)?;
        budget.charge()?;
        let result = self
            .database
            .as_ref()
            .map_err(|error| SqlError::Initialization(error.clone()))?
            .query(&normalized_query)?;
        result.to_data()
    }

    pub(crate) fn query_normalized_with_budget(
        &self,
        normalized_query: &str,
        budget: &SqlStatementBudget,
    ) -> Result<QueryResult, SqlError> {
        budget.charge()?;
        self.database
            .as_ref()
            .map_err(|error| SqlError::Initialization(error.clone()))?
            .query(normalized_query)
    }
}

pub(crate) fn normalize_query(query: &str) -> Result<String, SqlError> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(SqlError::QueryTooLarge);
    }
    if query.as_bytes().contains(&0) {
        return Err(SqlError::InteriorNul);
    }
    let normalized = query.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = normalized.trim();
    if normalized.is_empty() {
        return Err(SqlError::EmptyQuery);
    }
    Ok(normalized.to_string())
}

struct Database {
    raw: *mut ffi::sqlite3,
}

impl Database {
    fn from_snapshot(snapshot: &QuerySnapshot) -> Result<Self, SqlError> {
        let mut raw = ptr::null_mut();
        let rc = unsafe {
            ffi::sqlite3_open_v2(
                c":memory:".as_ptr(),
                &mut raw,
                ffi::SQLITE_OPEN_READWRITE
                    | ffi::SQLITE_OPEN_CREATE
                    | ffi::SQLITE_OPEN_MEMORY
                    | ffi::SQLITE_OPEN_NOMUTEX,
                ptr::null(),
            )
        };
        if rc != ffi::SQLITE_OK || raw.is_null() {
            let message = if raw.is_null() {
                format!("sqlite3_open_v2 returned {rc}")
            } else {
                unsafe { error_message(raw) }
            };
            if !raw.is_null() {
                unsafe {
                    ffi::sqlite3_close(raw);
                }
            }
            return Err(SqlError::Initialization(message));
        }
        let database = Self { raw };
        database.exec_batch(SCHEMA)?;
        database.exec_batch("BEGIN IMMEDIATE")?;
        if let Err(error) = database.populate(snapshot) {
            let _ = database.exec_batch("ROLLBACK");
            return Err(error);
        }
        database.exec_batch("COMMIT")?;
        database.install_limits();
        let rc = unsafe { ffi::sqlite3_set_authorizer(raw, Some(authorize), ptr::null_mut()) };
        if rc != ffi::SQLITE_OK {
            return Err(database.error());
        }
        Ok(database)
    }

    fn populate(&self, snapshot: &QuerySnapshot) -> Result<(), SqlError> {
        let mut statement = Statement::prepare(
            self,
            "INSERT INTO Resources (Key, Type, Custom, Id, Web, Url, Version, Status, Date, Name, Title, Experimental, Realm, Description, Purpose, Copyright, CopyrightLabel, derivation, standardStatus, kind, sdType, base, content, supplements, Json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;
        for row in &snapshot.resources {
            statement.bind_i64(1, row.key)?;
            statement.bind_text(2, &row.resource_type)?;
            statement.bind_i64(3, row.custom)?;
            statement.bind_text(4, &row.id)?;
            statement.bind_text(5, &row.web)?;
            statement.bind_opt_text(6, row.url.as_deref())?;
            statement.bind_opt_text(7, row.version.as_deref())?;
            statement.bind_opt_text(8, row.status.as_deref())?;
            statement.bind_opt_text(9, row.date.as_deref())?;
            statement.bind_opt_text(10, row.name.as_deref())?;
            statement.bind_opt_text(11, row.title.as_deref())?;
            statement.bind_opt_text(12, row.experimental.as_deref())?;
            statement.bind_opt_text(13, row.realm.as_deref())?;
            statement.bind_opt_text(14, row.description.as_deref())?;
            statement.bind_opt_text(15, row.purpose.as_deref())?;
            statement.bind_opt_text(16, row.copyright.as_deref())?;
            statement.bind_opt_text(17, row.copyright_label.as_deref())?;
            statement.bind_opt_text(18, row.derivation.as_deref())?;
            statement.bind_opt_text(19, row.standard_status.as_deref())?;
            statement.bind_opt_text(20, row.kind.as_deref())?;
            statement.bind_opt_text(21, row.sd_type.as_deref())?;
            statement.bind_opt_text(22, row.base.as_deref())?;
            statement.bind_opt_text(23, row.content.as_deref())?;
            statement.bind_opt_text(24, row.supplements.as_deref())?;
            statement.bind_blob(25, &row.json)?;
            statement.run_insert()?;
        }

        let mut statement = Statement::prepare(
            self,
            "INSERT INTO Properties (Key, ResourceKey, Code, Uri, Description, Type) VALUES (?, ?, ?, ?, ?, ?)",
        )?;
        for row in &snapshot.properties {
            statement.bind_i64(1, row.key)?;
            statement.bind_i64(2, row.resource_key)?;
            statement.bind_text(3, &row.code)?;
            statement.bind_opt_text(4, row.uri.as_deref())?;
            statement.bind_opt_text(5, row.description.as_deref())?;
            statement.bind_opt_text(6, row.property_type.as_deref())?;
            statement.run_insert()?;
        }

        let mut statement = Statement::prepare(
            self,
            "INSERT INTO Concepts (Key, ResourceKey, ParentKey, Code, Display, Definition) VALUES (?, ?, ?, ?, ?, ?)",
        )?;
        for row in &snapshot.concepts {
            statement.bind_i64(1, row.key)?;
            statement.bind_i64(2, row.resource_key)?;
            statement.bind_opt_i64(3, row.parent_key)?;
            statement.bind_opt_text(4, row.code.as_deref())?;
            statement.bind_opt_text(5, row.display.as_deref())?;
            statement.bind_opt_text(6, row.definition.as_deref())?;
            statement.run_insert()?;
        }

        let mut statement = Statement::prepare(
            self,
            "INSERT INTO ConceptProperties (Key, ResourceKey, ConceptKey, PropertyKey, Code, Value) VALUES (?, ?, ?, ?, ?, ?)",
        )?;
        for row in &snapshot.concept_properties {
            statement.bind_i64(1, row.key)?;
            statement.bind_i64(2, row.resource_key)?;
            statement.bind_i64(3, row.concept_key)?;
            statement.bind_opt_i64(4, row.property_key)?;
            statement.bind_opt_text(5, row.code.as_deref())?;
            statement.bind_opt_text(6, row.value.as_deref())?;
            statement.run_insert()?;
        }

        let mut statement = Statement::prepare(
            self,
            "INSERT INTO Designations (Key, ResourceKey, ConceptKey, UseSystem, UseCode, Lang, Value) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )?;
        for row in &snapshot.designations {
            statement.bind_i64(1, row.key)?;
            statement.bind_i64(2, row.resource_key)?;
            statement.bind_i64(3, row.concept_key)?;
            statement.bind_opt_text(4, row.use_system.as_deref())?;
            statement.bind_opt_text(5, row.use_code.as_deref())?;
            statement.bind_opt_text(6, row.language.as_deref())?;
            statement.bind_opt_text(7, row.value.as_deref())?;
            statement.run_insert()?;
        }

        let mut statement = Statement::prepare(
            self,
            "INSERT INTO ConceptMappings (Key, ResourceKey, SourceSystem, SourceVersion, SourceCode, Relationship, TargetSystem, TargetVersion, TargetCode) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?;
        for row in &snapshot.concept_mappings {
            statement.bind_i64(1, row.key)?;
            statement.bind_i64(2, row.resource_key)?;
            statement.bind_opt_text(3, row.source_system.as_deref())?;
            statement.bind_opt_text(4, row.source_version.as_deref())?;
            statement.bind_opt_text(5, row.source_code.as_deref())?;
            statement.bind_opt_text(6, row.relationship.as_deref())?;
            statement.bind_opt_text(7, row.target_system.as_deref())?;
            statement.bind_opt_text(8, row.target_version.as_deref())?;
            statement.bind_opt_text(9, row.target_code.as_deref())?;
            statement.run_insert()?;
        }

        Ok(())
    }

    fn install_limits(&self) {
        unsafe {
            ffi::sqlite3_limit(
                self.raw,
                ffi::SQLITE_LIMIT_LENGTH,
                MAX_RESULT_BYTES as c_int,
            );
            ffi::sqlite3_limit(
                self.raw,
                ffi::SQLITE_LIMIT_SQL_LENGTH,
                MAX_QUERY_BYTES as c_int,
            );
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_COLUMN, 256);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_EXPR_DEPTH, 100);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_COMPOUND_SELECT, 50);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_VDBE_OP, 2_000_000);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_FUNCTION_ARG, 100);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_ATTACHED, 0);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 10_000);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_VARIABLE_NUMBER, 0);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_TRIGGER_DEPTH, 0);
            ffi::sqlite3_limit(self.raw, ffi::SQLITE_LIMIT_WORKER_THREADS, 0);
        }
    }

    fn exec_batch(&self, sql: &str) -> Result<(), SqlError> {
        let sql = CString::new(sql).expect("static schema has no NUL");
        let mut error = ptr::null_mut();
        let rc =
            unsafe { ffi::sqlite3_exec(self.raw, sql.as_ptr(), None, ptr::null_mut(), &mut error) };
        if rc == ffi::SQLITE_OK {
            return Ok(());
        }
        let message = if error.is_null() {
            unsafe { error_message(self.raw) }
        } else {
            let message = unsafe { CStr::from_ptr(error).to_string_lossy().into_owned() };
            unsafe { ffi::sqlite3_free(error.cast()) };
            message
        };
        Err(SqlError::Sqlite(message))
    }

    fn query(&self, query: &str) -> Result<QueryResult, SqlError> {
        let mut statement = Statement::prepare(self, query)?;
        if !statement.tail_is_empty() {
            return Err(SqlError::MultipleStatements);
        }
        if unsafe { ffi::sqlite3_stmt_readonly(statement.raw) } == 0 {
            return Err(SqlError::NotReadOnly);
        }

        let budget = ProgressBudget {
            callbacks: Cell::new(0),
            exhausted: Cell::new(false),
        };
        unsafe {
            ffi::sqlite3_progress_handler(
                self.raw,
                PROGRESS_INTERVAL,
                Some(progress),
                (&budget as *const ProgressBudget).cast_mut().cast(),
            );
        }
        let result = statement.read_rows();
        unsafe {
            ffi::sqlite3_progress_handler(self.raw, 0, None, ptr::null_mut());
        }
        match result {
            Err(SqlError::Sqlite(_)) if budget.exhausted.get() => Err(SqlError::ExecutionBudget),
            other => other,
        }
    }

    fn error(&self) -> SqlError {
        SqlError::Sqlite(unsafe { error_message(self.raw) })
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_close(self.raw);
            }
        }
    }
}

struct Statement<'a> {
    database: &'a Database,
    raw: *mut ffi::sqlite3_stmt,
    tail: *const c_char,
}

impl<'a> Statement<'a> {
    fn prepare(database: &'a Database, sql: &str) -> Result<Self, SqlError> {
        let sql = CString::new(sql).map_err(|_| SqlError::InteriorNul)?;
        let mut raw = ptr::null_mut();
        let mut tail = ptr::null();
        let rc = unsafe {
            ffi::sqlite3_prepare_v3(
                database.raw,
                sql.as_ptr(),
                sql.as_bytes().len() as c_int,
                0,
                &mut raw,
                &mut tail,
            )
        };
        if rc != ffi::SQLITE_OK || raw.is_null() {
            let error = database.error();
            if !raw.is_null() {
                unsafe {
                    ffi::sqlite3_finalize(raw);
                }
            }
            return Err(error);
        }
        // The tail pointer points into `sql`; retain only whether it contains
        // another statement before dropping the CString. SQLite leaves trailing
        // comments in the tail even though they are not a second statement.
        let tail_nonempty = unsafe { tail_has_statement(CStr::from_ptr(tail).to_bytes()) };
        Ok(Self {
            database,
            raw,
            tail: if tail_nonempty {
                1usize as *const c_char
            } else {
                ptr::null()
            },
        })
    }

    fn tail_is_empty(&self) -> bool {
        self.tail.is_null()
    }

    fn bind_i64(&mut self, index: c_int, value: i64) -> Result<(), SqlError> {
        self.checked(unsafe { ffi::sqlite3_bind_int64(self.raw, index, value) })
    }

    fn bind_opt_i64(&mut self, index: c_int, value: Option<i64>) -> Result<(), SqlError> {
        match value {
            Some(value) => self.bind_i64(index, value),
            None => self.bind_null(index),
        }
    }

    fn bind_text(&mut self, index: c_int, value: &str) -> Result<(), SqlError> {
        self.checked(unsafe {
            ffi::sqlite3_bind_text(
                self.raw,
                index,
                value.as_ptr().cast(),
                value.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            )
        })
    }

    fn bind_opt_text(&mut self, index: c_int, value: Option<&str>) -> Result<(), SqlError> {
        match value {
            Some(value) => self.bind_text(index, value),
            None => self.bind_null(index),
        }
    }

    fn bind_blob(&mut self, index: c_int, value: &[u8]) -> Result<(), SqlError> {
        self.checked(unsafe {
            ffi::sqlite3_bind_blob(
                self.raw,
                index,
                value.as_ptr().cast(),
                value.len() as c_int,
                ffi::SQLITE_TRANSIENT(),
            )
        })
    }

    fn bind_null(&mut self, index: c_int) -> Result<(), SqlError> {
        self.checked(unsafe { ffi::sqlite3_bind_null(self.raw, index) })
    }

    fn checked(&self, rc: c_int) -> Result<(), SqlError> {
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(self.database.error())
        }
    }

    fn run_insert(&mut self) -> Result<(), SqlError> {
        let rc = unsafe { ffi::sqlite3_step(self.raw) };
        if rc != ffi::SQLITE_DONE {
            return Err(self.database.error());
        }
        self.checked(unsafe { ffi::sqlite3_reset(self.raw) })?;
        self.checked(unsafe { ffi::sqlite3_clear_bindings(self.raw) })
    }

    fn read_rows(&mut self) -> Result<QueryResult, SqlError> {
        let column_count = unsafe { ffi::sqlite3_column_count(self.raw) };
        let mut columns = Vec::with_capacity(column_count as usize);
        for index in 0..column_count {
            let name = unsafe { ffi::sqlite3_column_name(self.raw, index) };
            if name.is_null() {
                columns.push(String::new());
            } else {
                columns.push(unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() });
            }
        }
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        let mut cells = 0usize;
        loop {
            match unsafe { ffi::sqlite3_step(self.raw) } {
                ffi::SQLITE_ROW => {
                    if rows.len() == MAX_ROWS {
                        return Err(SqlError::RowLimit);
                    }
                    let mut row = Vec::with_capacity(column_count as usize);
                    for index in 0..column_count {
                        cells = cells.saturating_add(1);
                        if cells > MAX_RESULT_CELLS {
                            return Err(SqlError::CellLimit);
                        }
                        let value = unsafe { column_value(self.raw, index) };
                        bytes = bytes.saturating_add(value.byte_len());
                        if bytes > MAX_RESULT_BYTES {
                            return Err(SqlError::ResultLimit);
                        }
                        row.push(value);
                    }
                    rows.push(row);
                }
                ffi::SQLITE_DONE => break,
                _ => return Err(self.database.error()),
            }
        }
        Ok(QueryResult { columns, rows })
    }
}

fn tail_has_statement(mut bytes: &[u8]) -> bool {
    loop {
        bytes = bytes.trim_ascii_start();
        if let Some(rest) = bytes.strip_prefix(b"--") {
            bytes = rest
                .iter()
                .position(|byte| *byte == b'\n' || *byte == b'\r')
                .map(|end| &rest[end + 1..])
                .unwrap_or_default();
            continue;
        }
        if let Some(rest) = bytes.strip_prefix(b"/*") {
            bytes = rest
                .windows(2)
                .position(|window| window == b"*/")
                .map(|end| &rest[end + 2..])
                .unwrap_or_default();
            continue;
        }
        return !bytes.is_empty();
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                ffi::sqlite3_finalize(self.raw);
            }
        }
    }
}

struct ProgressBudget {
    callbacks: Cell<u32>,
    exhausted: Cell<bool>,
}

unsafe extern "C" fn progress(user: *mut c_void) -> c_int {
    let budget = &*(user.cast::<ProgressBudget>());
    let callbacks = budget.callbacks.get().saturating_add(1);
    budget.callbacks.set(callbacks);
    if callbacks > MAX_PROGRESS_CALLBACKS {
        budget.exhausted.set(true);
        1
    } else {
        0
    }
}

unsafe extern "C" fn authorize(
    _user: *mut c_void,
    action: c_int,
    argument_one: *const c_char,
    argument_two: *const c_char,
    _database: *const c_char,
    _trigger: *const c_char,
) -> c_int {
    match action {
        ffi::SQLITE_SELECT | ffi::SQLITE_RECURSIVE => ffi::SQLITE_OK,
        ffi::SQLITE_READ => {
            let table = if argument_one.is_null() {
                ""
            } else {
                CStr::from_ptr(argument_one).to_str().unwrap_or("")
            };
            if is_populated_table(table) {
                ffi::SQLITE_OK
            } else {
                ffi::SQLITE_DENY
            }
        }
        ffi::SQLITE_FUNCTION => {
            let name = if argument_two.is_null() {
                ""
            } else {
                CStr::from_ptr(argument_two).to_str().unwrap_or("")
            };
            if is_nondeterministic_function(name) {
                ffi::SQLITE_DENY
            } else {
                ffi::SQLITE_OK
            }
        }
        _ => ffi::SQLITE_DENY,
    }
}

fn is_populated_table(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "resources"
            | "properties"
            | "concepts"
            | "conceptproperties"
            | "designations"
            | "conceptmappings"
    )
}

fn is_nondeterministic_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "random"
            | "randomblob"
            | "changes"
            | "total_changes"
            | "last_insert_rowid"
            | "load_extension"
            | "sqlite_version"
            | "sqlite_source_id"
            | "current_time"
            | "current_date"
            | "current_timestamp"
            | "date"
            | "time"
            | "datetime"
            | "julianday"
            | "unixepoch"
            | "strftime"
            | "timediff"
    )
}

unsafe fn column_value(statement: *mut ffi::sqlite3_stmt, index: c_int) -> SqlValue {
    match ffi::sqlite3_column_type(statement, index) {
        ffi::SQLITE_NULL => SqlValue::Null,
        ffi::SQLITE_INTEGER => SqlValue::Integer(ffi::sqlite3_column_int64(statement, index)),
        ffi::SQLITE_FLOAT => SqlValue::Real(column_text(statement, index)),
        ffi::SQLITE_TEXT => SqlValue::Text(column_text(statement, index)),
        ffi::SQLITE_BLOB => {
            let length = ffi::sqlite3_column_bytes(statement, index).max(0) as usize;
            let data = ffi::sqlite3_column_blob(statement, index).cast::<u8>();
            if data.is_null() || length == 0 {
                SqlValue::Blob(Vec::new())
            } else {
                SqlValue::Blob(std::slice::from_raw_parts(data, length).to_vec())
            }
        }
        _ => SqlValue::Null,
    }
}

unsafe fn column_text(statement: *mut ffi::sqlite3_stmt, index: c_int) -> String {
    let length = ffi::sqlite3_column_bytes(statement, index).max(0) as usize;
    let data = ffi::sqlite3_column_text(statement, index);
    if data.is_null() || length == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(data, length)).into_owned()
    }
}

unsafe fn error_message(database: *mut ffi::sqlite3) -> String {
    let message = ffi::sqlite3_errmsg(database);
    if message.is_null() {
        "unknown SQLite error".to_string()
    } else {
        CStr::from_ptr(message).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn runtime() -> SqlRuntime {
        let code_system = json!({
            "resourceType": "CodeSystem",
            "id": "editor-stage",
            "url": "https://example.org/CodeSystem/editor-stage",
            "status": "active",
            "content": "complete",
            "concept": [
                {"code": "author", "display": "Author", "definition": "Edit source"},
                {"code": "explore", "display": "Explore", "definition": "Inspect FHIR"},
                {"code": "preview", "display": "Site preview", "definition": "Read output"}
            ]
        });
        SqlRuntime::from_resources([&code_system])
    }

    #[test]
    fn resources_concepts_and_json1_are_queryable() {
        let runtime = runtime();
        let result = runtime
            .query("SELECT c.Code, json_extract(r.Json, '$.id') AS Id FROM Resources r JOIN Concepts c ON c.ResourceKey = r.Key ORDER BY c.Code")
            .unwrap();
        assert_eq!(result.columns, ["Code", "Id"]);
        assert_eq!(result.rows.len(), 3);
        assert_eq!(result.rows[0][0], SqlValue::Text("author".into()));
        assert_eq!(result.rows[0][1], SqlValue::Text("editor-stage".into()));
    }

    #[test]
    fn sql_to_data_preserves_declared_scalar_shapes() {
        let runtime = runtime();
        let data = runtime
            .to_data(
                "SELECT Code AS code, Key AS ordinal, NULL AS absent FROM Concepts ORDER BY Key",
            )
            .unwrap();
        assert_eq!(data[0]["code"], "author");
        assert_eq!(data[0]["ordinal"], 1);
        assert!(data[0]["absent"].is_null());
    }

    #[test]
    fn unpopulated_compatibility_tables_fail_instead_of_looking_empty() {
        let runtime = runtime();
        for table in [
            "Metadata",
            "ValueSet_Codes",
            "CodeSystemList",
            "ValueSetList",
            "sqlite_master",
        ] {
            let error = runtime
                .query(&format!("SELECT * FROM {table}"))
                .expect_err(table);
            let message = error.to_string();
            assert!(
                message.contains("not authorized") || message.contains("prohibited"),
                "{table}: {error}"
            );
        }
    }

    #[test]
    fn writes_ambient_and_unbounded_queries_are_rejected() {
        let runtime = runtime();
        assert!(matches!(
            runtime.query("DELETE FROM Concepts"),
            Err(SqlError::Sqlite(_)) | Err(SqlError::NotReadOnly)
        ));
        assert!(runtime.query("PRAGMA table_info(Resources)").is_err());
        assert!(runtime.query("SELECT random()").is_err());
        assert!(runtime.query("ATTACH DATABASE 'x' AS x").is_err());
        assert!(matches!(
            runtime.query("SELECT 1; SELECT 2"),
            Err(SqlError::MultipleStatements)
        ));
    }

    #[test]
    fn trailing_comments_and_direct_duplicate_columns_follow_publisher_shape() {
        let runtime = runtime();
        assert_eq!(
            runtime
                .query("SELECT 1 AS value -- an explanatory tail")
                .unwrap()
                .rows[0][0],
            SqlValue::Integer(1)
        );
        assert_eq!(
            runtime
                .query("SELECT 1 AS value /* another explanatory tail */")
                .unwrap()
                .rows[0][0],
            SqlValue::Integer(1)
        );
        let duplicate = runtime.query("SELECT 1 AS value, 2 AS value").unwrap();
        assert_eq!(duplicate.columns, ["value", "value"]);
        assert!(matches!(
            duplicate.to_data(),
            Err(SqlError::DuplicateColumn(name)) if name == "value"
        ));
    }
}
