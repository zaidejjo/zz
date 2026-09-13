//! `std.sqlz` — SQLite via rusqlite (bundled). CANONICAL module name;
//! `std.db` is a zero-overhead alias exposing identical functionality.
//!
//! Compile-time parameterization contract:
//! - `sqlz.query(sql)` / `sqlz.exec(sql)` receive an *already-evaluated* SQL
//!   string plus the bound parameter values. The VM compiles `Fmt` with a
//!   dedicated `DbQuery` op that pushes the static template (with `{expr}`
//!   segments replaced by `?N` placeholders) and each bound value as
//!   separate stack items — never string-concatenated. These natives bind
//!   via `rusqlite::params_from_iter`, so user input can never alter the
//!   statement shape (100% injection-proof by construction).
//! - `sqlz.query` maps rows to ZZ structs positionally: column `i` → field
//!   `i`, with coercion int↔float, int→bool (0/1), text→str, null→`.none`
//!   for `Option` fields. The target struct name + field order come from
//!   `Interp::structs` via the checker's `Type::Array(Type::Struct)` return
//!   annotation (`let users: [User] = sqlz.query(...)`).

use std::sync::{Arc, Mutex};

use zz_runtime::value::{DbHandle, DbHandleInner, ObjectValue};
use zz_runtime::{EvalError, Interp, Span, Value};

use crate::natives::{arg, expect_str};

pub(crate) mod mysql_conn;
pub(crate) mod mysql_wire;
pub(crate) mod pg_conn;
pub(crate) mod pg_wire;

use mysql_conn::{MyConn, MyConnInfo};
use mysql_wire::{col_type_kind, MyKind, MyParam};
use pg_conn::{ConnInfo, PgConn, PgParam};
use pg_wire::{col_oid_kind, ColKind};

/// Concrete connections stored type-erased inside `Value::Db`.
/// `Sqlite` backs `sqlz.open` file/memory paths, `Pg` backs
/// `pg.connect` / `postgres://` URLs, `My` backs `my.connect` /
/// `mysql://` URLs. The shared `sqlz.query` / `sqlz.exec` natives
/// dispatch on this enum, so method syntax (`mydb.query(...)`) works
/// identically for all three backends.
#[derive(Debug)]
pub(crate) enum DbConn {
    Sqlite(Mutex<rusqlite::Connection>),
    Pg(Mutex<PgConn>),
    My(Mutex<MyConn>),
}

fn expect_db(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
    span: Span,
) -> Result<Arc<DbHandleInner>, EvalError> {
    match arg(args, i, name)? {
        Value::Db(h) => Ok(Arc::clone(&h.0)),
        other => Err(EvalError::new(
            format!("`{name}` expects a db handle, found `{other}`"),
            span,
        )),
    }
}

fn lock_handle<'a>(
    handle: &'a Arc<DbHandleInner>,
    name: &str,
    span: Span,
) -> Result<std::sync::MutexGuard<'a, Box<dyn std::any::Any + Send + Sync>>, EvalError> {
    handle
        .mutex
        .lock()
        .map_err(|e| EvalError::new(format!("`{name}`: lock poisoned: {e}"), span))
}

fn with_pg<T>(
    handle: &Arc<DbHandleInner>,
    name: &str,
    span: Span,
    f: impl FnOnce(&mut PgConn) -> Result<T, String>,
) -> Result<T, EvalError> {
    let guard = lock_handle(handle, name, span)?;
    let db = guard
        .downcast_ref::<DbConn>()
        .ok_or_else(|| EvalError::new(format!("`{name}`: invalid db handle"), span))?;
    match db {
        DbConn::Pg(conn) => {
            let mut clock = conn
                .lock()
                .map_err(|e| EvalError::new(format!("`{name}`: connection poisoned: {e}"), span))?;
            f(&mut clock).map_err(|e| EvalError::new(format!("`{name}` failed: {e}"), span))
        }
        DbConn::Sqlite(_) => Err(EvalError::new(
            format!("`{name}`: handle is a sqlite connection; use `sqlz.exec` / `sqlz.query`"),
            span,
        )),
        DbConn::My(_) => Err(EvalError::new(
            format!("`{name}`: handle is a mysql connection; use `my.exec` / `my.query`"),
            span,
        )),
    }
}

fn with_my<T>(
    handle: &Arc<DbHandleInner>,
    name: &str,
    span: Span,
    f: impl FnOnce(&mut MyConn) -> Result<T, String>,
) -> Result<T, EvalError> {
    let guard = lock_handle(handle, name, span)?;
    let db = guard
        .downcast_ref::<DbConn>()
        .ok_or_else(|| EvalError::new(format!("`{name}`: invalid db handle"), span))?;
    match db {
        DbConn::My(conn) => {
            let mut clock = conn
                .lock()
                .map_err(|e| EvalError::new(format!("`{name}`: connection poisoned: {e}"), span))?;
            f(&mut clock).map_err(|e| EvalError::new(format!("`{name}` failed: {e}"), span))
        }
        DbConn::Sqlite(_) => Err(EvalError::new(
            format!("`{name}`: handle is a sqlite connection; use `sqlz.exec` / `sqlz.query`"),
            span,
        )),
        DbConn::Pg(_) => Err(EvalError::new(
            format!("`{name}`: handle is a postgres connection; use `pg.exec` / `pg.query`"),
            span,
        )),
    }
}

/// `sqlz.open(path: str) -> db` — `:memory:`, file paths, and URL schemes:
/// `mysql://...` connects via the MySQL driver, `postgres://...` (or
/// `postgresql://...`) via the postgres driver.
pub(crate) fn db_open(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path =
        expect_str(args, 0, "sqlz.open").map_err(|e| EvalError::new(e.message.clone(), span))?;
    if path.starts_with("mysql://") {
        let info = MyConnInfo::parse(&path)
            .map_err(|e| EvalError::new(format!("sqlz.open: {e}"), span))?;
        let conn = MyConn::connect(&info)
            .map_err(|e| EvalError::new(format!("sqlz.open failed: {e}"), span))?;
        return Ok(Value::Db(DbHandle(Arc::new(DbHandleInner {
            mutex: Mutex::new(Box::new(DbConn::My(Mutex::new(conn)))),
        }))));
    }
    if path.starts_with("postgres://") || path.starts_with("postgresql://") {
        let info =
            ConnInfo::parse(&path).map_err(|e| EvalError::new(format!("sqlz.open: {e}"), span))?;
        let conn = PgConn::connect(&info)
            .map_err(|e| EvalError::new(format!("sqlz.open failed: {e}"), span))?;
        return Ok(Value::Db(DbHandle(Arc::new(DbHandleInner {
            mutex: Mutex::new(Box::new(DbConn::Pg(Mutex::new(conn)))),
        }))));
    }
    let conn = if path == ":memory:" {
        rusqlite::Connection::open_in_memory()
    } else {
        rusqlite::Connection::open(&path)
    }
    .map_err(|e| EvalError::new(format!("sqlz.open failed: {e}"), span))?;
    let inner = DbHandleInner {
        mutex: Mutex::new(Box::new(DbConn::Sqlite(Mutex::new(conn)))),
    };
    Ok(Value::Db(DbHandle(Arc::new(inner))))
}

/// Shared helper: split evaluated query args into (template, params).
/// The VM pushes `[template_str, p1, p2, ...]`; the tree-walker path
/// evaluates a plain `Str`/`Fmt` to a single string with no params.
fn split_query_args(args: &[Value]) -> (String, Vec<Value>) {
    let template = match args.first() {
        Some(Value::Str(s)) => s.to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let params = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        Vec::new()
    };
    (template, params)
}

fn bind_params(params: &[Value]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .map(|v| match v {
            Value::Int(i) => rusqlite::types::Value::Integer(*i),
            Value::Float(f) => rusqlite::types::Value::Real(*f),
            Value::Str(s) => rusqlite::types::Value::Text(s.to_string()),
            Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
            Value::Option(None) => rusqlite::types::Value::Null,
            Value::Option(Some(inner)) => match inner.as_ref() {
                Value::Int(i) => rusqlite::types::Value::Integer(*i),
                Value::Float(f) => rusqlite::types::Value::Real(*f),
                Value::Str(s) => rusqlite::types::Value::Text(s.to_string()),
                Value::Bool(b) => rusqlite::types::Value::Integer(i64::from(*b)),
                other => rusqlite::types::Value::Text(other.to_string()),
            },
            Value::Unit => rusqlite::types::Value::Null,
            other => rusqlite::types::Value::Text(other.to_string()),
        })
        .collect()
}

/// `sqlz.exec(db, sql, ...params) -> int` — rows changed.
/// Dispatches on the handle backend: rusqlite for sqlite, extended
/// protocol for postgres (`?N` template rewritten to `$N`), binary
/// prepared statements for mysql (`?N` rewritten to `?`).
pub(crate) fn db_exec(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "std.sqlz.exec", span)?;
    let (template, params) = split_query_args(&args[1..]);
    let guard = lock_handle(&handle, "std.sqlz.exec", span)?;
    let db = guard
        .downcast_ref::<DbConn>()
        .ok_or_else(|| EvalError::new("`std.sqlz.exec`: invalid db handle".to_string(), span))?;
    match db {
        DbConn::Sqlite(conn) => {
            let bound = bind_params(&params);
            let clock = conn.lock().map_err(|e| {
                EvalError::new(format!("`std.sqlz.exec`: connection poisoned: {e}"), span)
            })?;
            let mut stmt = clock
                .prepare(&template)
                .map_err(|e| EvalError::new(format!("`std.sqlz.exec` failed: {e}"), span))?;
            let n = stmt
                .execute(rusqlite::params_from_iter(bound.iter()))
                .map_err(|e| EvalError::new(format!("`std.sqlz.exec` failed: {e}"), span))?;
            Ok(Value::Int(n as i64))
        }
        DbConn::Pg(conn) => {
            let pg_params = to_pg_params(&params);
            let sql = rewrite_q_to_dollar(&template);
            let mut clock = conn.lock().map_err(|e| {
                EvalError::new(format!("`std.sqlz.exec`: connection poisoned: {e}"), span)
            })?;
            let n = clock
                .exec(&sql, &pg_params)
                .map_err(|e| EvalError::new(format!("`std.sqlz.exec` failed: {e}"), span))?;
            Ok(Value::Int(n))
        }
        DbConn::My(conn) => {
            let my_params = to_my_params(&params);
            let sql = rewrite_q_to_plain(&template);
            let mut clock = conn.lock().map_err(|e| {
                EvalError::new(format!("`std.sqlz.exec`: connection poisoned: {e}"), span)
            })?;
            let n = clock
                .exec(&sql, &my_params)
                .map_err(|e| EvalError::new(format!("`std.sqlz.exec` failed: {e}"), span))?;
            Ok(Value::Int(n))
        }
    }
}

/// Coerce one SQLite column value into a ZZ value for a struct field.
#[allow(dead_code)]
fn coerce_column(
    row: &rusqlite::Row,
    idx: usize,
    field: &str,
    struct_name: &str,
    span: Span,
) -> Result<Value, EvalError> {
    let val: rusqlite::types::Value = row.get(idx).map_err(|e| {
        EvalError::new(
            format!("db.query: column {idx} (`{field}`) read failed in `{struct_name}`: {e}"),
            span,
        )
    })?;
    Ok(match val {
        rusqlite::types::Value::Null => Value::Option(None),
        rusqlite::types::Value::Integer(i) => Value::Int(i),
        rusqlite::types::Value::Real(f) => Value::Float(f),
        rusqlite::types::Value::Text(s) => Value::Str(s.into()),
        rusqlite::types::Value::Blob(b) => {
            Value::Str(String::from_utf8_lossy(&b).into_owned().into())
        }
    })
}

/// `sqlz.query(db, sql, ...params) -> array` — rows mapped to structs.
///
/// Target struct resolution: the VM appends a trailing
/// `Value::Str("__struct:Name")` marker when the checker resolved an
/// `[Struct]` element type (emitted by the `DbQuery` op from
/// `span_types`). Absent the marker, falls back to positional dict-like
/// objects named `row`. Dispatches on the handle backend.
pub(crate) fn db_query(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "std.sqlz.query", span)?;
    let mut rest = args[1..].to_vec();
    let struct_name = pop_struct_marker(&mut rest);
    let (template, params) = split_query_args(&rest);
    let (obj_name, fields) = resolve_row_shape(interp, struct_name.as_deref(), span)?;

    let guard = lock_handle(&handle, "std.sqlz.query", span)?;
    let db = guard
        .downcast_ref::<DbConn>()
        .ok_or_else(|| EvalError::new("`std.sqlz.query`: invalid db handle".to_string(), span))?;
    let rows: Vec<Vec<Value>> = match db {
        DbConn::Sqlite(conn) => {
            let bound = bind_params(&params);
            let clock = conn.lock().map_err(|e| {
                EvalError::new(format!("`std.sqlz.query`: connection poisoned: {e}"), span)
            })?;
            let mut stmt = clock
                .prepare(&template)
                .map_err(|e| EvalError::new(format!("`std.sqlz.query` failed: {e}"), span))?;
            let col_count = stmt.column_count();
            check_arity(&fields, col_count, span)?;
            let raw = stmt
                .query_map(rusqlite::params_from_iter(bound.iter()), |row| {
                    (0..col_count)
                        .map(|i| row.get::<_, rusqlite::types::Value>(i))
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(|e| EvalError::new(format!("`std.sqlz.query` failed: {e}"), span))?;
            let mut out = Vec::new();
            for r in raw {
                let cols: Vec<rusqlite::types::Value> =
                    r.map_err(|e| EvalError::new(format!("`std.sqlz.query` failed: {e}"), span))?;
                out.push(cols.into_iter().map(sqlite_value_to_zz).collect());
            }
            out
        }
        DbConn::Pg(conn) => {
            let pg_params = to_pg_params(&params);
            let sql = rewrite_q_to_dollar(&template);
            let mut clock = conn.lock().map_err(|e| {
                EvalError::new(format!("`std.sqlz.query`: connection poisoned: {e}"), span)
            })?;
            let (cols, raw) = clock
                .query(&sql, &pg_params)
                .map_err(|e| EvalError::new(format!("`std.sqlz.query` failed: {e}"), span))?;
            check_arity(&fields, cols.len(), span)?;
            raw.into_iter()
                .map(|row| {
                    row.into_iter()
                        .zip(cols.iter())
                        .map(|(cell, col)| pg_cell_to_zz(cell, col.type_oid))
                        .collect()
                })
                .collect()
        }
        DbConn::My(conn) => {
            let my_params = to_my_params(&params);
            let sql = rewrite_q_to_plain(&template);
            let mut clock = conn.lock().map_err(|e| {
                EvalError::new(format!("`std.sqlz.query`: connection poisoned: {e}"), span)
            })?;
            let (cols, raw) = clock
                .query(&sql, &my_params)
                .map_err(|e| EvalError::new(format!("`std.sqlz.query` failed: {e}"), span))?;
            check_arity(&fields, cols.len(), span)?;
            raw.into_iter()
                .map(|row| {
                    row.into_iter()
                        .zip(cols.iter())
                        .map(|(cell, col)| my_cell_to_zz(cell, col.ftype))
                        .collect()
                })
                .collect()
        }
    };
    Ok(build_row_objects(&obj_name, &fields, rows))
}

/// Pop a trailing `__struct:Name` marker appended by the VM.
fn pop_struct_marker(rest: &mut Vec<Value>) -> Option<String> {
    match rest.last() {
        Some(Value::Str(s)) if s.starts_with("__struct:") => {
            let name = s["__struct:".len()..].to_string();
            rest.pop();
            Some(name)
        }
        _ => None,
    }
}

/// Resolve (object name, field order) for row mapping: explicit struct
/// from `Interp::structs`, or the positional `row{c0, ...}` fallback.
fn resolve_row_shape(
    interp: &Interp,
    struct_name: Option<&str>,
    span: Span,
) -> Result<(String, Vec<String>), EvalError> {
    match struct_name {
        Some(name) => {
            let fs = interp.structs.get(name).cloned().ok_or_else(|| {
                EvalError::new(format!("sqlz.query: unknown struct `{name}`"), span)
            })?;
            Ok((name.to_string(), fs))
        }
        None => Ok((String::from("row"), Vec::new())),
    }
}

/// Strict positional arity check when the target struct is known:
/// SQL column count must equal struct field count.
fn check_arity(fields: &[String], col_count: usize, span: Span) -> Result<(), EvalError> {
    if !fields.is_empty() && col_count != fields.len() {
        return Err(EvalError::new(
            format!(
                "sqlz.query: query returned {col_count} columns but struct has {} fields",
                fields.len()
            ),
            span,
        ));
    }
    Ok(())
}

/// Assemble row value-lists into ZZ objects: struct fields positionally
/// when known, `row{c0, ...}` dict-like objects otherwise.
fn build_row_objects(obj_name: &str, fields: &[String], rows: Vec<Vec<Value>>) -> Value {
    let mut out = Vec::with_capacity(rows.len());
    for cols in rows {
        let flds = if fields.is_empty() {
            cols.into_iter()
                .enumerate()
                .map(|(i, v)| (format!("c{i}"), v))
                .collect()
        } else {
            fields
                .iter()
                .cloned()
                .zip(cols)
                .collect::<Vec<(String, Value)>>()
        };
        out.push(Value::Object(Box::new(ObjectValue {
            name: obj_name.to_string(),
            fields: flds,
        })));
    }
    Value::Array(Box::new(out))
}

/// Convert a ZZ bound value to a Postgres text-format parameter.
fn to_pg_params(params: &[Value]) -> Vec<PgParam> {
    params.iter().map(zz_to_pg_param).collect()
}

fn zz_to_pg_param(v: &Value) -> PgParam {
    match v {
        Value::Int(i) => PgParam::Text(i.to_string()),
        Value::Float(f) => PgParam::Text(float_to_pg_text(*f)),
        Value::Str(s) => PgParam::Text(s.to_string()),
        Value::Bool(b) => PgParam::Text(if *b {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        Value::Option(None) | Value::Unit => PgParam::Null,
        Value::Option(Some(inner)) => zz_to_pg_param(inner),
        other => PgParam::Text(other.to_string()),
    }
}

fn float_to_pg_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        }
    } else {
        format!("{f:?}")
    }
}

/// Rewrite the VM's `?N` placeholders to Postgres `$N`. The `DbQuery` op
/// emits `?1..?N` in order; scanning char-by-char keeps `?10` (two digits)
/// intact where naive replacement would corrupt it.
fn rewrite_q_to_dollar(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '?' && chars.peek().is_some_and(|p| p.is_ascii_digit()) {
            out.push('$');
            while let Some(d) = chars.peek() {
                if d.is_ascii_digit() {
                    out.push(*d);
                    chars.next();
                } else {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Map one text-format cell to a ZZ value using the column type OID.
fn pg_cell_to_zz(cell: Option<Vec<u8>>, type_oid: u32) -> Value {
    let bytes = match cell {
        None => return Value::Option(None),
        Some(b) => b,
    };
    let text = String::from_utf8_lossy(&bytes);
    match col_oid_kind(type_oid) {
        ColKind::Int => text
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(text.into_owned().into())),
        ColKind::Float => text
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .unwrap_or_else(|_| Value::Str(text.into_owned().into())),
        ColKind::Bool => match text.trim() {
            "t" | "true" | "TRUE" | "1" => Value::Bool(true),
            "f" | "false" | "FALSE" | "0" => Value::Bool(false),
            _ => Value::Str(text.into_owned().into()),
        },
        ColKind::Text => Value::Str(text.into_owned().into()),
    }
}

fn sqlite_value_to_zz(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Null => Value::Option(None),
        rusqlite::types::Value::Integer(i) => Value::Int(i),
        rusqlite::types::Value::Real(f) => Value::Float(f),
        rusqlite::types::Value::Text(s) => Value::Str(s.into()),
        rusqlite::types::Value::Blob(b) => {
            Value::Str(String::from_utf8_lossy(&b).into_owned().into())
        }
    }
}

/// `sqlz.close(db) -> unit` — explicit close (also dropped automatically).
/// Works for both backends (the postgres socket closes on drop).
pub(crate) fn db_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let _ = expect_db(args, 0, "std.sqlz.close", span)?;
    Ok(Value::Unit)
}

/// `pg.connect(conninfo: str) -> db` — postgres via wire protocol v3.0.
///
/// Accepts URL form (`postgres://user:pass@host:port/dbname`) or keyword
/// form (`host=.. port=.. dbname=.. user=.. password=..`). Blocks the
/// calling thread (with timeouts); run inside `task.spawn` for concurrency.
pub(crate) fn pg_connect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let conninfo =
        expect_str(args, 0, "pg.connect").map_err(|e| EvalError::new(e.message.clone(), span))?;
    let info =
        ConnInfo::parse(&conninfo).map_err(|e| EvalError::new(format!("pg.connect: {e}"), span))?;
    let conn = PgConn::connect(&info)
        .map_err(|e| EvalError::new(format!("pg.connect failed: {e}"), span))?;
    let inner = DbHandleInner {
        mutex: Mutex::new(Box::new(DbConn::Pg(Mutex::new(conn)))),
    };
    Ok(Value::Db(DbHandle(Arc::new(inner))))
}

/// Shared explicit-receiver query path for `pg.query(db, sql, ...params)`.
/// Requires a postgres handle (use `sqlz.query` for backend dispatch).
fn pg_query_impl(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
    name: &str,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, name, span)?;
    let mut rest = args[1..].to_vec();
    let struct_name = pop_struct_marker(&mut rest);
    let (template, params) = split_query_args(&rest);
    let (obj_name, fields) = resolve_row_shape(interp, struct_name.as_deref(), span)?;
    let pg_params = to_pg_params(&params);
    let sql = rewrite_q_to_dollar(&template);
    let (cols, raw) = with_pg(&handle, name, span, |conn| conn.query(&sql, &pg_params))?;
    check_arity(&fields, cols.len(), span)?;
    let rows: Vec<Vec<Value>> = raw
        .into_iter()
        .map(|row| {
            row.into_iter()
                .zip(cols.iter())
                .map(|(cell, col)| pg_cell_to_zz(cell, col.type_oid))
                .collect()
        })
        .collect();
    Ok(build_row_objects(&obj_name, &fields, rows))
}

/// `pg.query(db, sql, ...params) -> array` — postgres rows to structs.
pub(crate) fn pg_query(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    pg_query_impl(interp, args, span, "pg.query")
}

/// `pg.exec(db, sql, ...params) -> int` — affected rows via command tag.
pub(crate) fn pg_exec(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "pg.exec", span)?;
    let (template, params) = split_query_args(&args[1..]);
    let pg_params = to_pg_params(&params);
    let sql = rewrite_q_to_dollar(&template);
    let n = with_pg(&handle, "pg.exec", span, |conn| conn.exec(&sql, &pg_params))?;
    Ok(Value::Int(n))
}

/// `pg.close(db) -> unit` — sends `Terminate` (best effort).
pub(crate) fn pg_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "pg.close", span)?;
    with_pg(&handle, "pg.close", span, |conn| {
        conn.terminate();
        Ok(())
    })?;
    Ok(Value::Unit)
}

/// Convert a ZZ bound value to a MySQL binary-protocol parameter.
fn to_my_params(params: &[Value]) -> Vec<MyParam> {
    params.iter().map(zz_to_my_param).collect()
}

fn zz_to_my_param(v: &Value) -> MyParam {
    match v {
        Value::Int(i) => MyParam::Int(*i),
        Value::Float(f) => MyParam::Float(*f),
        Value::Str(s) => MyParam::Str(s.as_bytes().to_vec()),
        Value::Bool(b) => MyParam::Bool(*b),
        Value::Option(None) | Value::Unit => MyParam::Null,
        Value::Option(Some(inner)) => zz_to_my_param(inner),
        other => MyParam::Str(other.to_string().into_bytes()),
    }
}

/// Rewrite the VM's `?N` placeholders to bare MySQL `?` (positional).
/// The `DbQuery` op emits `?1..?N` in order, which already matches
/// `COM_STMT_EXECUTE` positional binding. Digit-aware so `?10` stays one
/// placeholder where naive replacement would corrupt it.
fn rewrite_q_to_plain(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '?' && chars.peek().is_some_and(|p| p.is_ascii_digit()) {
            out.push('?');
            while chars.peek().is_some_and(|p| p.is_ascii_digit()) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Map one binary-protocol cell to a ZZ value using the column type.
/// Integer/float types decode little-endian; everything else (dates,
/// decimals, strings, blobs) arrives as text.
fn my_cell_to_zz(cell: Option<Vec<u8>>, ftype: u8) -> Value {
    let bytes = match cell {
        None => return Value::Option(None),
        Some(b) => b,
    };
    match col_type_kind(ftype) {
        MyKind::Int => {
            let n = match bytes.len() {
                1 => i64::from(bytes[0]),
                2 => i64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
                4 => i64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                8 => i64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8])),
                _ => {
                    return Value::Str(String::from_utf8_lossy(&bytes).into_owned().into());
                }
            };
            // Unsigned 64-bit values above i64::MAX stay honest as text.
            if bytes.len() == 8 && n < 0 {
                let u = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
                if u > i64::MAX as u64 {
                    return Value::Str(u.to_string().into());
                }
            }
            Value::Int(n)
        }
        MyKind::Float => {
            let f = match bytes.len() {
                4 => f64::from(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                8 => f64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8])),
                _ => {
                    return Value::Str(String::from_utf8_lossy(&bytes).into_owned().into());
                }
            };
            Value::Float(f)
        }
        MyKind::Text => {
            let text = String::from_utf8_lossy(&bytes);
            // NUMERIC/DECIMAL arrives as text: prefer Float when numeric.
            if ftype == mysql_wire::TYPE_NEWDECIMAL || ftype == mysql_wire::TYPE_DECIMAL {
                if let Ok(f) = text.trim().parse::<f64>() {
                    return Value::Float(f);
                }
            }
            Value::Str(text.into_owned().into())
        }
    }
}

/// `my.connect(conninfo: str) -> db` — mysql via wire protocol.
///
/// Accepts URL form (`mysql://user:pass@host:port/dbname`) or keyword
/// form (`host=.. port=.. dbname=.. user=.. password=..`). Blocks the
/// calling thread (with timeouts); run inside `task.spawn` for concurrency.
pub(crate) fn my_connect(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let conninfo =
        expect_str(args, 0, "my.connect").map_err(|e| EvalError::new(e.message.clone(), span))?;
    let info = MyConnInfo::parse(&conninfo)
        .map_err(|e| EvalError::new(format!("my.connect: {e}"), span))?;
    let conn = MyConn::connect(&info)
        .map_err(|e| EvalError::new(format!("my.connect failed: {e}"), span))?;
    let inner = DbHandleInner {
        mutex: Mutex::new(Box::new(DbConn::My(Mutex::new(conn)))),
    };
    Ok(Value::Db(DbHandle(Arc::new(inner))))
}

/// Shared explicit-receiver query path for `my.query(db, sql, ...params)`.
/// Requires a mysql handle (use `sqlz.query` for backend dispatch).
fn my_query_impl(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
    name: &str,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, name, span)?;
    let mut rest = args[1..].to_vec();
    let struct_name = pop_struct_marker(&mut rest);
    let (template, params) = split_query_args(&rest);
    let (obj_name, fields) = resolve_row_shape(interp, struct_name.as_deref(), span)?;
    let my_params = to_my_params(&params);
    let sql = rewrite_q_to_plain(&template);
    let (cols, raw) = with_my(&handle, name, span, |conn| conn.query(&sql, &my_params))?;
    check_arity(&fields, cols.len(), span)?;
    let rows: Vec<Vec<Value>> = raw
        .into_iter()
        .map(|row| {
            row.into_iter()
                .zip(cols.iter())
                .map(|(cell, col)| my_cell_to_zz(cell, col.ftype))
                .collect()
        })
        .collect();
    Ok(build_row_objects(&obj_name, &fields, rows))
}

/// `my.query(db, sql, ...params) -> array` — mysql rows to structs.
pub(crate) fn my_query(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    my_query_impl(interp, args, span, "my.query")
}

/// `my.exec(db, sql, ...params) -> int` — affected rows.
pub(crate) fn my_exec(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "my.exec", span)?;
    let (template, params) = split_query_args(&args[1..]);
    let my_params = to_my_params(&params);
    let sql = rewrite_q_to_plain(&template);
    let n = with_my(&handle, "my.exec", span, |conn| conn.exec(&sql, &my_params))?;
    Ok(Value::Int(n))
}

/// `my.close(db) -> unit` — sends `COM_QUIT` (best effort).
pub(crate) fn my_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "my.close", span)?;
    with_my(&handle, "my.close", span, |conn| {
        conn.terminate();
        Ok(())
    })?;
    Ok(Value::Unit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn read_frame(s: &mut std::net::TcpStream) -> Vec<u8> {
        let mut hdr = [0u8; 4];
        s.read_exact(&mut hdr).unwrap();
        let len = (hdr[0] as usize) | ((hdr[1] as usize) << 8) | ((hdr[2] as usize) << 16);
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        body
    }

    fn send_frame(s: &mut std::net::TcpStream, seq: u8, body: &[u8]) {
        s.write_all(&mysql_wire::frame(seq, body)).unwrap();
    }

    fn coldef(name: &str, ty: u8) -> Vec<u8> {
        let mut b = Vec::new();
        for s in ["def", "", "t", ""] {
            let mut l = Vec::new();
            mysql_wire::write_lenenc(s.len() as u64, &mut l);
            b.extend_from_slice(&l);
            b.extend_from_slice(s.as_bytes());
        }
        let mut l = Vec::new();
        mysql_wire::write_lenenc(name.len() as u64, &mut l);
        b.extend_from_slice(&l);
        b.extend_from_slice(name.as_bytes());
        l.clear();
        mysql_wire::write_lenenc(0, &mut l);
        b.extend_from_slice(&l);
        b.push(0x0c);
        b.extend_from_slice(&33u16.to_le_bytes());
        b.extend_from_slice(&256u32.to_le_bytes());
        b.push(ty);
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(0);
        b.extend_from_slice(&[0, 0]);
        b
    }

    /// Mock MySQL server: native-password handshake, then one
    /// prepare/execute round answering two rows. Sends the observed
    /// prepared SQL back for the rewrite assertion.
    fn mock_mysql() -> (std::net::SocketAddr, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            s.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
            // HandshakeV10 (mysql_native_password).
            let mut hs = vec![10u8];
            hs.extend_from_slice(b"8.0.36-mock\0");
            hs.extend_from_slice(&7u32.to_le_bytes());
            hs.extend_from_slice(b"abcdefgh");
            hs.push(0);
            hs.extend_from_slice(&0x8207u16.to_le_bytes());
            hs.push(45);
            hs.extend_from_slice(&2u16.to_le_bytes());
            hs.extend_from_slice(&0x0008u16.to_le_bytes());
            hs.push(21);
            hs.extend_from_slice(&[0u8; 10]);
            hs.extend_from_slice(b"ijklmnopqrst");
            hs.push(0);
            hs.extend_from_slice(b"mysql_native_password\0");
            send_frame(&mut s, 0, &hs);
            let _resp = read_frame(&mut s);
            send_frame(&mut s, 2, &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]);
            // COM_STMT_PREPARE
            let prep = read_frame(&mut s);
            assert_eq!(prep[0], mysql_wire::COM_STMT_PREPARE);
            tx.send(String::from_utf8(prep[1..].to_vec()).unwrap())
                .unwrap();
            let mut po = vec![0x00];
            po.extend_from_slice(&7u32.to_le_bytes());
            po.extend_from_slice(&2u16.to_le_bytes());
            po.extend_from_slice(&1u16.to_le_bytes());
            po.push(0);
            po.extend_from_slice(&0u16.to_le_bytes());
            send_frame(&mut s, 0, &po);
            send_frame(&mut s, 1, &coldef("?", mysql_wire::TYPE_VAR_STRING));
            send_frame(&mut s, 2, &[0xFE, 0, 0, 0x02, 0x00]);
            send_frame(&mut s, 3, &coldef("id", mysql_wire::TYPE_LONG));
            send_frame(&mut s, 4, &coldef("name", mysql_wire::TYPE_VAR_STRING));
            send_frame(&mut s, 5, &[0xFE, 0, 0, 0x02, 0x00]);
            // COM_STMT_EXECUTE
            let exec = read_frame(&mut s);
            assert_eq!(exec[0], mysql_wire::COM_STMT_EXECUTE);
            assert_eq!(&exec[1..5], &7u32.to_le_bytes());
            send_frame(&mut s, 0, &[0x02]);
            send_frame(&mut s, 1, &coldef("id", mysql_wire::TYPE_LONG));
            send_frame(&mut s, 2, &coldef("name", mysql_wire::TYPE_VAR_STRING));
            send_frame(&mut s, 3, &[0xFE, 0, 0, 0x02, 0x00]);
            // Row (7, "carol").
            let mut r1 = vec![0x00u8, 0x00];
            r1.extend_from_slice(&7i32.to_le_bytes());
            r1.push(5);
            r1.extend_from_slice(b"carol");
            send_frame(&mut s, 4, &r1);
            // Row (NULL, "dave").
            let mut r2 = vec![0x00u8, 0x04];
            r2.push(4);
            r2.extend_from_slice(b"dave");
            send_frame(&mut s, 5, &r2);
            send_frame(&mut s, 6, &[0xFE, 0, 0, 0x02, 0x00]);
        });
        (addr, rx)
    }

    #[test]
    fn mysql_native_query_maps_structs() {
        let (addr, rx) = mock_mysql();
        let mut interp = Interp::new();
        interp.structs.insert(
            "User".to_string(),
            vec!["id".to_string(), "name".to_string()],
        );
        let span = Span::new(0, 0);
        // Connect through the real native.
        let mut cargs = vec![Value::Str(
            format!("host=127.0.0.1 port={} user=u", addr.port()).into(),
        )];
        let handle = my_connect(&mut interp, &mut cargs, span).unwrap();
        // Query through the real native: template with `?1` (as the VM's
        // DbQuery op emits), one bound param, struct marker.
        let mut qargs = vec![
            handle,
            Value::Str("SELECT id, name FROM t WHERE id = ?1".to_string().into()),
            Value::Int(7),
            Value::Str("__struct:User".to_string().into()),
        ];
        let out = my_query(&mut interp, &mut qargs, span).unwrap();
        match out {
            Value::Array(rows) => {
                assert_eq!(rows.len(), 2);
                match &rows[0] {
                    Value::Object(o) => {
                        assert_eq!(o.name, "User");
                        assert_eq!(o.fields[0], ("id".to_string(), Value::Int(7)));
                        assert_eq!(
                            o.fields[1],
                            ("name".to_string(), Value::Str("carol".to_string().into()))
                        );
                    }
                    other => panic!("expected object, got {other:?}"),
                }
                // NULL id maps to `.none`.
                match &rows[1] {
                    Value::Object(o) => {
                        assert_eq!(o.fields[0].1, Value::Option(None));
                    }
                    other => panic!("expected object, got {other:?}"),
                }
            }
            other => panic!("expected array, got {other:?}"),
        }
        // The prepared SQL carries a bare `?` (rewritten from `?1`).
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "SELECT id, name FROM t WHERE id = ?"
        );
    }

    #[test]
    fn qmark_rewrite_is_digit_aware() {
        assert_eq!(rewrite_q_to_plain("SELECT ?1, ?2"), "SELECT ?, ?");
        assert_eq!(rewrite_q_to_plain("SELECT ?10"), "SELECT ?");
        assert_eq!(rewrite_q_to_plain("no params"), "no params");
        assert_eq!(rewrite_q_to_plain("a ? b"), "a ? b");
    }
}
