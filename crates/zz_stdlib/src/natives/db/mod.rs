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

pub(crate) mod pg_conn;
pub(crate) mod pg_wire;

use pg_conn::{ConnInfo, PgConn, PgParam};
use pg_wire::{col_oid_kind, ColKind};

/// Concrete connections stored type-erased inside `Value::Db`.
/// `Sqlite` backs `sqlz.open`; `Pg` backs `pg.connect`. The shared
/// `sqlz.query` / `sqlz.exec` natives dispatch on this enum, so method
/// syntax (`mydb.query(...)`) works identically for both backends.
#[derive(Debug)]
pub(crate) enum DbConn {
    Sqlite(Mutex<rusqlite::Connection>),
    Pg(Mutex<PgConn>),
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
    }
}

/// `db.open(path: str) -> db` — `:memory:` supported.
pub(crate) fn db_open(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let path =
        expect_str(args, 0, "std.db.open").map_err(|e| EvalError::new(e.message.clone(), span))?;
    let conn = if path == ":memory:" {
        rusqlite::Connection::open_in_memory()
    } else {
        rusqlite::Connection::open(&path)
    }
    .map_err(|e| EvalError::new(format!("std.db.open failed: {e}"), span))?;
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
/// protocol for postgres (`?N` template rewritten to `$N`).
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
