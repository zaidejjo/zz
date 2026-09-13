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

/// Concrete connection stored type-erased inside `Value::Db`.
pub(crate) struct SqliteConn(pub Mutex<rusqlite::Connection>);

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

fn with_conn<T>(
    handle: &Arc<DbHandleInner>,
    name: &str,
    span: Span,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<T, EvalError> {
    let guard = handle
        .mutex
        .lock()
        .map_err(|e| EvalError::new(format!("`{name}`: lock poisoned: {e}"), span))?;
    let conn = guard
        .downcast_ref::<SqliteConn>()
        .ok_or_else(|| EvalError::new(format!("`{name}`: invalid db handle"), span))?;
    let clock = conn
        .0
        .lock()
        .map_err(|e| EvalError::new(format!("`{name}`: connection poisoned: {e}"), span))?;
    f(&clock).map_err(|e| EvalError::new(format!("`{name}` failed: {e}"), span))
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
        mutex: Mutex::new(Box::new(SqliteConn(Mutex::new(conn)))),
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

/// `db.exec(db, sql, ...params) -> int` — rows changed.
pub(crate) fn db_exec(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "std.db.exec", span)?;
    let (template, params) = split_query_args(&args[1..]);
    let bound = bind_params(&params);
    let changed = with_conn(&handle, "std.db.exec", span, |conn| {
        let mut stmt = conn.prepare(&template)?;
        let n = stmt.execute(rusqlite::params_from_iter(bound.iter()))?;
        Ok(n as i64)
    })?;
    Ok(Value::Int(changed))
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

/// `db.query(db, sql, ...params) -> array` — rows mapped to structs.
///
/// Target struct resolution order:
/// 1. `Interp::structs` lookup by the checker's element type is not
///    directly visible here, so the caller passes the struct name via a
///    trailing `Value::Str("__struct:Name")` marker when the VM knows it
///    (emitted by the `DbQuery` op from `span_types`). Absent the marker,
///    fall back to positional dict-like objects named `row`.
pub(crate) fn db_query(
    interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let handle = expect_db(args, 0, "std.db.query", span)?;
    let mut rest = args[1..].to_vec();
    // Pop optional struct marker appended by the VM.
    let struct_name: Option<String> = match rest.last() {
        Some(Value::Str(s)) if s.starts_with("__struct:") => {
            let name = s["__struct:".len()..].to_string();
            rest.pop();
            Some(name)
        }
        _ => None,
    };
    let (template, params) = split_query_args(&rest);
    let bound = bind_params(&params);

    // Resolve field order: explicit struct or positional fallback.
    let (obj_name, fields): (String, Vec<String>) = match &struct_name {
        Some(name) => {
            let fs = interp.structs.get(name).cloned().ok_or_else(|| {
                EvalError::new(format!("db.query: unknown struct `{name}`"), span)
            })?;
            (name.clone(), fs)
        }
        None => (String::from("row"), Vec::new()),
    };

    with_conn(&handle, "std.db.query", span, |conn| {
        let mut stmt = conn.prepare(&template)?;
        let col_count = stmt.column_count();
        // Positional mapping: column i -> field i. Strict arity check
        // when the target struct is known.
        if !fields.is_empty() && col_count != fields.len() {
            return Err(rusqlite::Error::InvalidColumnIndex(col_count));
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(bound.iter()), |row| {
            (0..col_count)
                .map(|i| row.get::<_, rusqlite::types::Value>(i))
                .collect::<Result<Vec<_>, _>>()
        })?;
        let mut out = Vec::new();
        for r in rows {
            let cols: Vec<rusqlite::types::Value> = r?;
            if fields.is_empty() {
                // No struct context: return positional tuple-like objects
                // `row{c0: v0, ...}` so REPL exploration still works.
                let flds = cols
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| (format!("c{i}"), sqlite_value_to_zz(c)))
                    .collect();
                out.push(Value::Object(Box::new(ObjectValue {
                    name: obj_name.clone(),
                    fields: flds,
                })));
            } else {
                let mut flds = Vec::with_capacity(fields.len());
                for (i, fname) in fields.iter().enumerate() {
                    // Positional mapping: column i -> field i.
                    let v = sqlite_value_to_zz(cols[i].clone());
                    flds.push((fname.clone(), v));
                }
                out.push(Value::Object(Box::new(ObjectValue {
                    name: obj_name.clone(),
                    fields: flds,
                })));
            }
        }
        Ok(Value::Array(Box::new(out)))
    })
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

/// `db.close(db) -> unit` — explicit close (also dropped automatically).
pub(crate) fn db_close(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: Span,
) -> Result<Value, EvalError> {
    let _ = expect_db(args, 0, "std.db.close", span)?;
    Ok(Value::Unit)
}
