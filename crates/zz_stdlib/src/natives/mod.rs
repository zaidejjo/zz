//! Standard library native implementations, consumed by the interpreter.
//!
//! Native functions take `&mut Vec<Value>` (not a slice) because
//! `std.vec.push` must grow the argument vector.

#![allow(clippy::ptr_arg)]

use std::collections::HashMap;

use zz_runtime::{EvalError, NativeEntry, Value};

pub(crate) mod builtins;
pub(crate) mod env;
pub(crate) mod fs;
pub(crate) mod http;
pub(crate) mod io;
pub(crate) mod iterators;
pub(crate) mod json;
pub(crate) mod math;
pub(crate) mod option_mod;
pub(crate) mod result_mod;
pub(crate) mod str_mod;
pub(crate) mod time;
pub(crate) mod vec_mod;

/// All standard library native functions, keyed by qualified name.
pub fn stdlib_natives() -> HashMap<String, NativeEntry> {
    let mut m = HashMap::new();

    // std.io
    m.insert(
        "std.io.printz".into(),
        NativeEntry {
            arity: 1,
            f: io::printz,
        },
    );
    m.insert(
        "std.io.println".into(),
        NativeEntry {
            arity: 1,
            f: io::println,
        },
    );
    m.insert(
        "std.io.read_line".into(),
        NativeEntry {
            arity: 0,
            f: io::read_line,
        },
    );

    // Top-level builtins — available without import.
    m.insert(
        "print".into(),
        NativeEntry {
            arity: 1,
            f: io::printz,
        },
    );
    m.insert(
        "println".into(),
        NativeEntry {
            arity: 1,
            f: io::println,
        },
    );
    m.insert(
        "input".into(),
        NativeEntry {
            arity: 1,
            f: io::read_line,
        },
    );

    // Range and iterator builtins
    m.insert(
        "range".into(),
        NativeEntry {
            arity: 3,
            f: iterators::range,
        },
    );
    m.insert(
        "len".into(),
        NativeEntry {
            arity: 1,
            f: iterators::len,
        },
    );
    m.insert(
        "map".into(),
        NativeEntry {
            arity: 2,
            f: iterators::map,
        },
    );
    m.insert(
        "filter".into(),
        NativeEntry {
            arity: 2,
            f: iterators::filter,
        },
    );
    m.insert(
        "enumerate".into(),
        NativeEntry {
            arity: 1,
            f: iterators::enumerate,
        },
    );
    m.insert(
        "zip".into(),
        NativeEntry {
            arity: 2,
            f: iterators::zip,
        },
    );

    // std.str
    m.insert(
        "std.str.length".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_length,
        },
    );
    m.insert(
        "std.str.split".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_split,
        },
    );
    m.insert(
        "std.str.contains".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_contains,
        },
    );
    // str.* methods (for method dispatch: "hello".trim())
    m.insert(
        "str.trim".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_trim,
        },
    );
    m.insert(
        "str.to_upper".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_to_upper,
        },
    );
    m.insert(
        "str.to_lower".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_to_lower,
        },
    );
    m.insert(
        "str.split".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_split,
        },
    );
    m.insert(
        "str.contains".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_contains,
        },
    );
    m.insert(
        "str.replace".into(),
        NativeEntry {
            arity: 3,
            f: str_mod::str_replace,
        },
    );
    m.insert(
        "str.starts_with".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_starts_with,
        },
    );
    m.insert(
        "str.ends_with".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_ends_with,
        },
    );

    // std.vec
    m.insert(
        "std.vec.len".into(),
        NativeEntry {
            arity: 1,
            f: vec_mod::vec_len,
        },
    );
    m.insert(
        "std.vec.push".into(),
        NativeEntry {
            arity: 2,
            f: vec_mod::vec_push,
        },
    );
    m.insert(
        "std.vec.pop".into(),
        NativeEntry {
            arity: 1,
            f: vec_mod::vec_pop,
        },
    );
    // vec.* methods (for method dispatch: [1,2].push(3))
    m.insert(
        "vec.len".into(),
        NativeEntry {
            arity: 1,
            f: vec_mod::vec_len,
        },
    );
    m.insert(
        "vec.push".into(),
        NativeEntry {
            arity: 2,
            f: vec_mod::vec_push,
        },
    );
    m.insert(
        "vec.pop".into(),
        NativeEntry {
            arity: 1,
            f: vec_mod::vec_pop,
        },
    );
    m.insert(
        "vec.reverse".into(),
        NativeEntry {
            arity: 1,
            f: vec_mod::vec_reverse,
        },
    );
    m.insert(
        "vec.join".into(),
        NativeEntry {
            arity: 2,
            f: vec_mod::vec_join,
        },
    );
    m.insert(
        "vec.contains".into(),
        NativeEntry {
            arity: 2,
            f: vec_mod::vec_contains,
        },
    );
    m.insert(
        "vec.sort".into(),
        NativeEntry {
            arity: 1,
            f: vec_mod::vec_sort,
        },
    );
    m.insert(
        "vec.insert".into(),
        NativeEntry {
            arity: 3,
            f: vec_mod::vec_insert,
        },
    );
    m.insert(
        "vec.remove".into(),
        NativeEntry {
            arity: 2,
            f: vec_mod::vec_remove,
        },
    );

    // option.* methods (for method dispatch: .some(1).unwrap_or(0))
    m.insert(
        "option.unwrap".into(),
        NativeEntry {
            arity: 1,
            f: option_mod::option_unwrap,
        },
    );
    m.insert(
        "option.unwrap_or".into(),
        NativeEntry {
            arity: 2,
            f: option_mod::option_unwrap_or,
        },
    );
    m.insert(
        "option.expect".into(),
        NativeEntry {
            arity: 2,
            f: option_mod::option_expect,
        },
    );

    // result.* methods (for method dispatch: .ok(1).unwrap_or(0))
    m.insert(
        "result.unwrap".into(),
        NativeEntry {
            arity: 1,
            f: result_mod::result_unwrap,
        },
    );
    m.insert(
        "result.unwrap_or".into(),
        NativeEntry {
            arity: 2,
            f: result_mod::result_unwrap_or,
        },
    );
    m.insert(
        "result.expect".into(),
        NativeEntry {
            arity: 2,
            f: result_mod::result_expect,
        },
    );

    // std.json
    m.insert(
        "std.json.parse".into(),
        NativeEntry {
            arity: 1,
            f: json::json_parse,
        },
    );
    m.insert(
        "std.json.stringify".into(),
        NativeEntry {
            arity: 1,
            f: json::json_stringify,
        },
    );
    m.insert(
        "std.json.get".into(),
        NativeEntry {
            arity: 2,
            f: json::json_get,
        },
    );
    m.insert(
        "std.json.as_str".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_str,
        },
    );
    m.insert(
        "std.json.as_int".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_int,
        },
    );
    m.insert(
        "std.json.as_float".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_float,
        },
    );
    m.insert(
        "std.json.as_bool".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_bool,
        },
    );

    // std.http
    m.insert(
        "std.http.server".into(),
        NativeEntry {
            arity: 0,
            f: http::http_server,
        },
    );
    m.insert(
        "std.http.get".into(),
        NativeEntry {
            arity: 3,
            f: http::http_get,
        },
    );
    m.insert(
        "std.http.post".into(),
        NativeEntry {
            arity: 3,
            f: http::http_post,
        },
    );
    m.insert(
        "std.http.handle".into(),
        NativeEntry {
            arity: 4,
            f: http::http_handle,
        },
    );
    m.insert(
        "std.http.listen".into(),
        NativeEntry {
            arity: 2,
            f: http::http_listen,
        },
    );

    // std.fs
    m.insert(
        "std.fs.read_file".into(),
        NativeEntry {
            arity: 1,
            f: fs::fs_read_file,
        },
    );
    m.insert(
        "std.fs.write_file".into(),
        NativeEntry {
            arity: 2,
            f: fs::fs_write_file,
        },
    );
    m.insert(
        "std.fs.exists".into(),
        NativeEntry {
            arity: 1,
            f: fs::fs_exists,
        },
    );

    // std.env
    m.insert(
        "std.env.get_var".into(),
        NativeEntry {
            arity: 1,
            f: env::env_get_var,
        },
    );
    m.insert(
        "std.env.args".into(),
        NativeEntry {
            arity: 0,
            f: env::env_args,
        },
    );

    // std.math
    m.insert(
        "std.math.abs".into(),
        NativeEntry {
            arity: 1,
            f: math::math_abs,
        },
    );
    m.insert(
        "std.math.floor".into(),
        NativeEntry {
            arity: 1,
            f: math::math_floor,
        },
    );
    m.insert(
        "std.math.ceil".into(),
        NativeEntry {
            arity: 1,
            f: math::math_ceil,
        },
    );
    m.insert(
        "std.math.sqrt".into(),
        NativeEntry {
            arity: 1,
            f: math::math_sqrt,
        },
    );
    m.insert(
        "std.math.pow".into(),
        NativeEntry {
            arity: 2,
            f: math::math_pow,
        },
    );
    m.insert(
        "std.math.random".into(),
        NativeEntry {
            arity: 0,
            f: math::math_random,
        },
    );

    // std.time
    m.insert(
        "std.time.now_ms".into(),
        NativeEntry {
            arity: 0,
            f: time::time_now_ms,
        },
    );
    m.insert(
        "std.time.sleep_ms".into(),
        NativeEntry {
            arity: 1,
            f: time::time_sleep_ms,
        },
    );

    // Built-in: `typeof(v)` — the runtime type name of any value.
    m.insert(
        "typeof".into(),
        NativeEntry {
            arity: 1,
            f: builtins::typeof_fn,
        },
    );

    // Built-in conversions.
    m.insert(
        "str".into(),
        NativeEntry {
            arity: 1,
            f: builtins::conv_str,
        },
    );
    m.insert(
        "int".into(),
        NativeEntry {
            arity: 1,
            f: builtins::conv_int,
        },
    );
    m.insert(
        "float".into(),
        NativeEntry {
            arity: 1,
            f: builtins::conv_float,
        },
    );

    // Built-in: `append(arr, val)` — mutates array in-place, returns unit.
    m.insert(
        "append".into(),
        NativeEntry {
            arity: 2,
            f: builtins::append_fn,
        },
    );

    m
}

// --- helpers ---------------------------------------------------------------

pub(crate) fn arg<'a>(
    args: &'a mut Vec<Value>,
    i: usize,
    name: &str,
) -> Result<&'a Value, EvalError> {
    args.get(i).ok_or_else(|| {
        EvalError::new(
            format!("missing argument `{name}` for native function"),
            zz_runtime::Span::new(0, 0),
        )
    })
}

pub(crate) fn expect_str(args: &mut Vec<Value>, i: usize, name: &str) -> Result<String, EvalError> {
    match arg(args, i, name)? {
        Value::Str(s) => Ok(s.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects a string, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn expect_array(
    args: &mut Vec<Value>,
    i: usize,
    name: &str,
) -> Result<Vec<Value>, EvalError> {
    match arg(args, i, name)? {
        Value::Array(vs) => Ok(vs.clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an array, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn expect_func(args: &mut Vec<Value>, i: usize, name: &str) -> Result<Value, EvalError> {
    match arg(args, i, name)? {
        Value::Func(f) => Ok(Value::Func(f.clone())),
        Value::Native(n) => Ok(Value::Native(n.clone())),
        other => Err(EvalError::new(
            format!("`{name}` expects a function, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn expect_int(args: &mut Vec<Value>, i: usize, name: &str) -> Result<i64, EvalError> {
    match arg(args, i, name)? {
        Value::Int(n) => Ok(*n),
        other => Err(EvalError::new(
            format!("`{name}` expects an integer, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

#[cfg(test)]
mod tests;
