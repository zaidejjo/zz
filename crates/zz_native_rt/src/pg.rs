//! `std.sqlz.postgres` implementation for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls [`connect`],
//! [`exec`], [`query`], [`close`] through thin `zz_stdlib` adapters, while
//! AOT binaries call the `extern "C"` `zz_pg_*_raw` functions below. The
//! wire protocol itself ([`crate::pg_conn`], [`crate::pg_wire`]) is shared
//! verbatim — same auth, TLS, types, and errors on both engines.
//!
//! Connections live in the process pool under tag `"pg"` and cross the
//! FFI as `u64` ids (`0` is invalid); closing drops the pool entry, so
//! use-after-close degrades to empty results instead of dangling.
//!
//! AOT conventions (mirroring the SQLite natives' leniency rather than the
//! VM's raises, so `sqlz.*` programs run unchanged on either engine):
//! connect failure yields id `0`; `query` builds row dicts keyed by
//! column name (VM cell mapping) and failures yield `[]`; `exec` yields
//! affected rows, `-1` on failure (the C dispatcher flags it for
//! transactions and reports `0`).
//!
//! Bind parameters arrive as a `zz_value` array (the lowerer's DbQuery
//! template + binds shape): int/float/bool/str bind as text, `.some`
//! unwraps one level, none/unit and anything else bind Null (same
//! degradation as the AOT SQLite path).

use std::sync::{Arc, Mutex};

use crate::cabi::CValue;
use crate::pg_conn::{ConnInfo, PgConn, PgParam, RawRows};
use crate::pg_wire::{col_oid_kind, ColKind};
use crate::placeholders::{render, PlaceholderStyle};
use crate::{alloc, drop_handle, payload};

/// Pool tag for PG connections. Selects the `pg.*` method namespace.
pub const TAG: &str = "pg";

/// Tag constants (mirror the C `zz_tag` order; see `cabi`).
const TAG_FLOAT: u32 = 2;
const TAG_BOOL: u32 = 3;
const TAG_DICT: u32 = 6;
const TAG_OPTION_SOME: u32 = 9;

extern "C" {
    fn zz_dict_new() -> CValue;
    fn zz_index_set(obj: CValue, idx: CValue, item: CValue, err: *mut std::ffi::c_int);
}

/// Build a `ZZ_FLOAT` value (payload is the f64 bits, like the C union).
fn cvalue_float(f: f64) -> CValue {
    CValue {
        tag: TAG_FLOAT,
        _pad: 0,
        payload: f.to_bits(),
    }
}

/// Build a `ZZ_BOOL` value.
fn cvalue_bool(b: bool) -> CValue {
    CValue {
        tag: TAG_BOOL,
        _pad: 0,
        payload: u64::from(b),
    }
}

/// Insert into a `ZZ_DICT` built by [`zz_dict_new`]: the dict retains the
/// key and adopts the value, so release the key temp afterwards.
fn dict_insert(dict: CValue, key: &str, val: CValue) {
    if dict.tag != TAG_DICT {
        return;
    }
    let mut err = 0;
    let k = crate::cabi::cvalue_str(key.as_bytes());
    // SAFETY: `dict` came from a `zz_dict_new` value in this call frame;
    // `k` is released below (the dict retains its own ref).
    unsafe {
        zz_index_set(dict, k, val, &mut err);
        crate::cabi::zz_value_release(k);
    }
}

/// Connect + authenticate, storing the connection in the pool.
pub fn connect(conninfo: &str) -> Result<crate::Handle, String> {
    let info = ConnInfo::parse(conninfo).map_err(|e| format!("pg.connect: {e}"))?;
    let conn = PgConn::connect(&info).map_err(|e| format!("pg.connect failed: {e}"))?;
    Ok(alloc(TAG, Arc::new(Mutex::new(conn))))
}

/// Run `f` on the connection for `id`. `None` for unknown/dropped ids.
pub fn with<T>(id: u64, f: impl FnOnce(&Mutex<PgConn>) -> T) -> Option<T> {
    payload::<Mutex<PgConn>>(id, TAG).map(|conn| f(&conn))
}

/// Execute a statement; returns affected-row count from the tag.
pub fn exec(conn: &Mutex<PgConn>, sql: &str, params: &[PgParam]) -> Result<i64, String> {
    let sql = render(sql, PlaceholderStyle::Dollar);
    conn.lock()
        .map_err(|_| "pg.exec: connection lock poisoned".to_string())?
        .exec(&sql, params)
}

/// Query rows; returns (columns, rows) with raw text bytes.
pub fn query(conn: &Mutex<PgConn>, sql: &str, params: &[PgParam]) -> Result<RawRows, String> {
    let sql = render(sql, PlaceholderStyle::Dollar);
    conn.lock()
        .map_err(|_| "pg.query: connection lock poisoned".to_string())?
        .query(&sql, params)
}

/// Terminate (best effort) and drop the pool entry.
pub fn close(id: u64) {
    if let Some(conn) = payload::<Mutex<PgConn>>(id, TAG) {
        if let Ok(mut guard) = conn.lock() {
            guard.terminate();
        }
    }
    drop_handle(id);
}

/// Float formatting identical to the VM (`NaN`/`Infinity` spellings PG
/// expects; `{:?}` keeps whole floats round-trippable).
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

/// Read one bind parameter from its `zz_value` (recursive through `.some`).
fn cvalue_to_pg_param(v: CValue) -> PgParam {
    match v.tag {
        crate::cabi::TAG_INT => PgParam::Text((v.payload as i64).to_string()),
        TAG_FLOAT => PgParam::Text(float_to_pg_text(f64::from_bits(v.payload))),
        TAG_BOOL => PgParam::Text(if v.payload == 0 { "false" } else { "true" }.to_string()),
        crate::cabi::TAG_STR => match crate::cabi::cvalue_to_string(v) {
            Some(s) => PgParam::Text(s),
            None => PgParam::Null,
        },
        crate::cabi::TAG_OPTION_NONE => PgParam::Null,
        TAG_OPTION_SOME => {
            // Payload points at the inner zz_value: unwrap one level.
            let inner = unsafe { *(v.payload as *const CValue) };
            cvalue_to_pg_param(inner)
        }
        // Unit, arrays, dicts, handles, ... bind Null, matching the AOT
        // SQLite path's degradation for exotic binds.
        _ => PgParam::Null,
    }
}

/// Read the binds array into Postgres text-format parameters.
fn cvalue_to_pg_params(binds: *const CValue, nbinds: usize) -> Vec<PgParam> {
    if binds.is_null() {
        return Vec::new();
    }
    // SAFETY: the C caller guarantees `nbinds` readable values.
    let slice = unsafe { std::slice::from_raw_parts(binds, nbinds) };
    slice.iter().map(|v| cvalue_to_pg_param(*v)).collect()
}

/// Map one text-format cell to a `zz_value` using the column type OID
/// (identical mapping to the VM's `pg_cell_to_zz`).
fn pg_cell_to_cvalue(cell: Option<Vec<u8>>, type_oid: u32) -> CValue {
    let bytes = match cell {
        None => return CValue::none(),
        Some(b) => b,
    };
    let text = String::from_utf8_lossy(&bytes);
    match col_oid_kind(type_oid) {
        ColKind::Int => match text.trim().parse::<i64>() {
            Ok(i) => CValue::int(i),
            Err(_) => crate::cabi::cvalue_str(text.as_bytes()),
        },
        ColKind::Float => match text.trim().parse::<f64>() {
            Ok(f) => cvalue_float(f),
            Err(_) => crate::cabi::cvalue_str(text.as_bytes()),
        },
        ColKind::Bool => match text.trim() {
            "t" | "true" | "TRUE" | "1" => cvalue_bool(true),
            "f" | "false" | "FALSE" | "0" => cvalue_bool(false),
            _ => crate::cabi::cvalue_str(text.as_bytes()),
        },
        ColKind::Text => crate::cabi::cvalue_str(text.as_bytes()),
    }
}

/// Build the row-dict array for a query result (owns everything built).
fn rows_to_cvalue(cols: &[crate::pg_wire::ColDesc], raw: Vec<Vec<Option<Vec<u8>>>>) -> CValue {
    // SAFETY: constructors are provided by the linked AOT program.
    unsafe {
        let out = crate::cabi::zz_array_new();
        let Some(arr_ptr) = out.as_array_ptr() else {
            return out;
        };
        for row in raw {
            let dict = zz_dict_new();
            for (cell, col) in row.into_iter().zip(cols.iter()) {
                dict_insert(dict, &col.name, pg_cell_to_cvalue(cell, col.type_oid));
            }
            crate::cabi::zz_array_push(arr_ptr, dict);
        }
        out
    }
}

fn read_str(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the C caller guarantees `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// `zz_pg_connect_raw(info, len) -> pool id` (`0` on failure).
#[no_mangle]
pub extern "C" fn zz_pg_connect_raw(info_ptr: *const u8, info_len: usize) -> u64 {
    let info = read_str(info_ptr, info_len);
    match connect(&info) {
        Ok(h) => h.id,
        Err(_) => 0,
    }
}

/// `zz_pg_exec_raw(id, sql, len, binds, nbinds) -> affected rows`
/// (`-1` on any failure — the C dispatcher maps it to the transaction
/// error flag and reports `0`, mirroring the SQLite natives).
#[no_mangle]
pub extern "C" fn zz_pg_exec_raw(
    id: u64,
    sql_ptr: *const u8,
    sql_len: usize,
    binds: *const CValue,
    nbinds: usize,
) -> i64 {
    let sql = read_str(sql_ptr, sql_len);
    let params = cvalue_to_pg_params(binds, nbinds);
    with(id, |conn| exec(conn, &sql, &params))
        .and_then(|r| r.ok())
        .unwrap_or(-1)
}

/// `zz_pg_query_raw(id, sql, len, binds, nbinds) -> [row]` (empty array
/// on any failure, mirroring the SQLite natives).
#[no_mangle]
pub extern "C" fn zz_pg_query_raw(
    id: u64,
    sql_ptr: *const u8,
    sql_len: usize,
    binds: *const CValue,
    nbinds: usize,
) -> CValue {
    // SAFETY: array constructor is provided by the linked AOT program.
    let empty = unsafe { crate::cabi::zz_array_new() };
    if id == 0 {
        return empty;
    }
    let sql = read_str(sql_ptr, sql_len);
    let params = cvalue_to_pg_params(binds, nbinds);
    match with(id, |conn| query(conn, &sql, &params)) {
        Some(Ok((cols, rows))) => rows_to_cvalue(&cols, rows),
        _ => empty,
    }
}

/// `zz_pg_close_raw(id)` — terminate (best effort) and drop the entry.
#[no_mangle]
pub extern "C" fn zz_pg_close_raw(id: u64) {
    if id != 0 {
        close(id);
    }
}
