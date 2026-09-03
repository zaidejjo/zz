//! Standard library native implementations, consumed by the interpreter.
//!
//! Native functions take `&mut Vec<Value>` (not a slice) because
//! `std.vec.push` must grow the argument vector.

#![allow(clippy::ptr_arg)]

use std::collections::HashMap;

use zz_runtime::{EvalError, NativeEntry, Value};

pub(crate) mod builtins;
pub(crate) mod encoding;
pub(crate) mod env;
pub(crate) mod fs;
pub(crate) mod http;
pub(crate) mod io;
pub(crate) mod iterators;
pub(crate) mod json;
pub(crate) mod math;
pub(crate) mod net;
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
        "str.length".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_length,
        },
    );
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
    m.insert(
        "str.join".into(),
        NativeEntry {
            arity: 2,
            f: str_mod::str_join,
        },
    );
    m.insert(
        "str.trim_start".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_trim_start,
        },
    );
    m.insert(
        "str.trim_end".into(),
        NativeEntry {
            arity: 1,
            f: str_mod::str_trim_end,
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
    // vec.append — alias for vec.push, same semantics
    m.insert(
        "vec.append".into(),
        NativeEntry {
            arity: 2,
            f: vec_mod::vec_push,
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
        "std.json.null".into(),
        NativeEntry {
            arity: 0,
            f: json::json_null,
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

    // std.encoding
    m.insert(
        "std.encoding.base64_encode".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_base64_encode,
        },
    );
    m.insert(
        "std.encoding.base64_decode".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_base64_decode,
        },
    );
    m.insert(
        "std.encoding.hex_encode".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_hex_encode,
        },
    );
    m.insert(
        "std.encoding.hex_decode".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_hex_decode,
        },
    );
    m.insert(
        "std.encoding.url_encode".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_url_encode,
        },
    );
    m.insert(
        "std.encoding.url_decode".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_url_decode,
        },
    );

    // std.http — Client
    m.insert(
        "std.http.get".into(),
        NativeEntry {
            arity: 2,
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
        "std.http.put".into(),
        NativeEntry {
            arity: 3,
            f: http::http_put,
        },
    );
    m.insert(
        "std.http.delete".into(),
        NativeEntry {
            arity: 2,
            f: http::http_delete,
        },
    );

    // std.http — Response methods (dispatched via method_namespace "http")
    m.insert(
        "http.status".into(),
        NativeEntry {
            arity: 1,
            f: http::http_response_status,
        },
    );
    m.insert(
        "http.text".into(),
        NativeEntry {
            arity: 1,
            f: http::http_response_text,
        },
    );
    m.insert(
        "http.json".into(),
        NativeEntry {
            arity: 1,
            f: http::http_response_json,
        },
    );
    m.insert(
        "http.headers".into(),
        NativeEntry {
            arity: 1,
            f: http::http_response_headers,
        },
    );

    // std.http — Server (per-route model)
    m.insert(
        "std.http.server".into(),
        NativeEntry {
            arity: 0,
            f: http::http_server,
        },
    );
    m.insert(
        "std.http.route_get".into(),
        NativeEntry {
            arity: 3,
            f: http::http_route_get,
        },
    );
    m.insert(
        "std.http.route_post".into(),
        NativeEntry {
            arity: 3,
            f: http::http_route_post,
        },
    );
    m.insert(
        "std.http.route_put".into(),
        NativeEntry {
            arity: 3,
            f: http::http_route_put,
        },
    );
    m.insert(
        "std.http.route_delete".into(),
        NativeEntry {
            arity: 3,
            f: http::http_route_delete,
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

    // std.http — Phase 5B features
    m.insert(
        "std.http.log".into(),
        NativeEntry {
            arity: 2,
            f: http::http_log,
        },
    );
    m.insert(
        "std.http.pipe".into(),
        NativeEntry {
            arity: 2,
            f: http::http_pipe,
        },
    );
    m.insert(
        "std.http.serve_dir".into(),
        NativeEntry {
            arity: 2,
            f: http::http_serve_dir,
        },
    );
    m.insert(
        "std.http.test".into(),
        NativeEntry {
            arity: 4,
            f: http::http_test,
        },
    );
    m.insert(
        "std.http.param".into(),
        NativeEntry {
            arity: 2,
            f: http::http_param,
        },
    );
    m.insert(
        "std.http.query".into(),
        NativeEntry {
            arity: 1,
            f: http::http_query,
        },
    );
    m.insert(
        "std.http.header".into(),
        NativeEntry {
            arity: 2,
            f: http::http_header,
        },
    );
    m.insert(
        "std.http.body_json".into(),
        NativeEntry {
            arity: 1,
            f: http::http_body_json,
        },
    );
    m.insert(
        "std.http.body_form".into(),
        NativeEntry {
            arity: 1,
            f: http::http_body_form,
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
    m.insert(
        "std.fs.read_to_string".into(),
        NativeEntry {
            arity: 1,
            f: fs::fs_read_file,
        },
    );
    m.insert(
        "std.fs.write".into(),
        NativeEntry {
            arity: 2,
            f: fs::fs_write_file,
        },
    );
    m.insert(
        "std.fs.remove_file".into(),
        NativeEntry {
            arity: 1,
            f: fs::fs_remove_file,
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
        "std.env.var".into(),
        NativeEntry {
            arity: 1,
            f: env::env_var,
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

    // ── std.math constants ──
    m.insert(
        "std.math.PI".into(),
        NativeEntry {
            arity: 0,
            f: math::math_pi,
        },
    );
    m.insert(
        "std.math.E".into(),
        NativeEntry {
            arity: 0,
            f: math::math_e,
        },
    );
    m.insert(
        "std.math.TAU".into(),
        NativeEntry {
            arity: 0,
            f: math::math_tau,
        },
    );
    m.insert(
        "std.math.INF".into(),
        NativeEntry {
            arity: 0,
            f: math::math_inf,
        },
    );
    m.insert(
        "std.math.NAN".into(),
        NativeEntry {
            arity: 0,
            f: math::math_nan,
        },
    );

    // ── std.math utilities & rounding ──
    m.insert(
        "std.math.round".into(),
        NativeEntry {
            arity: 1,
            f: math::math_round,
        },
    );
    m.insert(
        "std.math.trunc".into(),
        NativeEntry {
            arity: 1,
            f: math::math_trunc,
        },
    );
    m.insert(
        "std.math.clamp".into(),
        NativeEntry {
            arity: 3,
            f: math::math_clamp,
        },
    );
    m.insert(
        "std.math.signum".into(),
        NativeEntry {
            arity: 1,
            f: math::math_signum,
        },
    );
    m.insert(
        "std.math.hypot".into(),
        NativeEntry {
            arity: 2,
            f: math::math_hypot,
        },
    );
    m.insert(
        "std.math.is_nan".into(),
        NativeEntry {
            arity: 1,
            f: math::math_is_nan,
        },
    );
    m.insert(
        "std.math.is_inf".into(),
        NativeEntry {
            arity: 1,
            f: math::math_is_inf,
        },
    );

    // ── std.math number theory ──
    m.insert(
        "std.math.root".into(),
        NativeEntry {
            arity: 2,
            f: math::math_root,
        },
    );
    m.insert(
        "std.math.isqrt".into(),
        NativeEntry {
            arity: 1,
            f: math::math_isqrt,
        },
    );
    m.insert(
        "std.math.factorial".into(),
        NativeEntry {
            arity: 1,
            f: math::math_factorial,
        },
    );
    m.insert(
        "std.math.gcd".into(),
        NativeEntry {
            arity: 2,
            f: math::math_gcd,
        },
    );
    m.insert(
        "std.math.lcm".into(),
        NativeEntry {
            arity: 2,
            f: math::math_lcm,
        },
    );

    // ── std.math trigonometry ──
    m.insert(
        "std.math.sin".into(),
        NativeEntry {
            arity: 1,
            f: math::math_sin,
        },
    );
    m.insert(
        "std.math.cos".into(),
        NativeEntry {
            arity: 1,
            f: math::math_cos,
        },
    );
    m.insert(
        "std.math.tan".into(),
        NativeEntry {
            arity: 1,
            f: math::math_tan,
        },
    );
    m.insert(
        "std.math.asin".into(),
        NativeEntry {
            arity: 1,
            f: math::math_asin,
        },
    );
    m.insert(
        "std.math.acos".into(),
        NativeEntry {
            arity: 1,
            f: math::math_acos,
        },
    );
    m.insert(
        "std.math.atan".into(),
        NativeEntry {
            arity: 1,
            f: math::math_atan,
        },
    );
    m.insert(
        "std.math.sin_deg".into(),
        NativeEntry {
            arity: 1,
            f: math::math_sin_deg,
        },
    );
    m.insert(
        "std.math.cos_deg".into(),
        NativeEntry {
            arity: 1,
            f: math::math_cos_deg,
        },
    );
    m.insert(
        "std.math.tan_deg".into(),
        NativeEntry {
            arity: 1,
            f: math::math_tan_deg,
        },
    );
    m.insert(
        "std.math.to_radians".into(),
        NativeEntry {
            arity: 1,
            f: math::math_to_radians,
        },
    );
    m.insert(
        "std.math.to_degrees".into(),
        NativeEntry {
            arity: 1,
            f: math::math_to_degrees,
        },
    );

    // ── std.math logarithms & exponents ──
    m.insert(
        "std.math.log".into(),
        NativeEntry {
            arity: 1,
            f: math::math_log,
        },
    );
    m.insert(
        "std.math.log10".into(),
        NativeEntry {
            arity: 1,
            f: math::math_log10,
        },
    );
    m.insert(
        "std.math.exp".into(),
        NativeEntry {
            arity: 1,
            f: math::math_exp,
        },
    );

    // ── std.math linear algebra ──
    m.insert(
        "std.math.dot_product".into(),
        NativeEntry {
            arity: 2,
            f: math::math_dot_product,
        },
    );
    m.insert(
        "std.math.magnitude".into(),
        NativeEntry {
            arity: 1,
            f: math::math_magnitude,
        },
    );
    m.insert(
        "std.math.matrix_mul".into(),
        NativeEntry {
            arity: 2,
            f: math::math_matrix_mul,
        },
    );

    // ── std.math statistics & random ──
    m.insert(
        "std.math.mean".into(),
        NativeEntry {
            arity: 1,
            f: math::math_mean,
        },
    );
    m.insert(
        "std.math.median".into(),
        NativeEntry {
            arity: 1,
            f: math::math_median,
        },
    );
    m.insert(
        "std.math.rand_range".into(),
        NativeEntry {
            arity: 2,
            f: math::math_rand_range,
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

    // std.net — TCP networking
    m.insert(
        "std.net.tcp_connect".into(),
        NativeEntry {
            arity: 2,
            f: net::tcp_connect,
        },
    );
    m.insert(
        "std.net.tcp_listen".into(),
        NativeEntry {
            arity: 1,
            f: net::tcp_listen,
        },
    );
    m.insert(
        "std.net.tcp_accept".into(),
        NativeEntry {
            arity: 1,
            f: net::tcp_accept,
        },
    );
    m.insert(
        "std.net.tcp_write".into(),
        NativeEntry {
            arity: 2,
            f: net::tcp_write,
        },
    );
    m.insert(
        "std.net.tcp_read".into(),
        NativeEntry {
            arity: 2,
            f: net::tcp_read,
        },
    );
    m.insert(
        "std.net.tcp_readline".into(),
        NativeEntry {
            arity: 1,
            f: net::tcp_readline,
        },
    );
    m.insert(
        "std.net.tcp_close".into(),
        NativeEntry {
            arity: 1,
            f: net::tcp_close,
        },
    );
    m.insert(
        "std.net.peer_addr".into(),
        NativeEntry {
            arity: 1,
            f: net::peer_addr,
        },
    );
    m.insert(
        "std.net.local_addr".into(),
        NativeEntry {
            arity: 1,
            f: net::local_addr,
        },
    );
    m.insert(
        "std.net.set_read_timeout".into(),
        NativeEntry {
            arity: 2,
            f: net::set_read_timeout,
        },
    );
    m.insert(
        "std.net.set_write_timeout".into(),
        NativeEntry {
            arity: 2,
            f: net::set_write_timeout,
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
