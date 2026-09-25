//! Standard library native implementations, consumed by the interpreter.
//!
//! Native functions take `&mut Vec<Value>` (not a slice) because
//! `std.vec.push` must grow the argument vector.

#![allow(clippy::ptr_arg)]

use std::collections::HashMap;

use zz_runtime::{EvalError, NativeEntry, Value};

pub(crate) mod args;
pub(crate) mod assert;
pub(crate) mod builtins;
pub(crate) mod concurrency;
pub(crate) mod crypto;
pub(crate) mod db;
pub(crate) mod encoding;
pub(crate) mod env;
pub mod fs;
pub(crate) mod http;
pub(crate) mod io;
pub(crate) mod iterators;
pub(crate) mod json;
pub(crate) mod log;
pub(crate) mod math;
pub(crate) mod net;
pub(crate) mod option_mod;
pub(crate) mod process;
pub(crate) mod regexp;
pub(crate) mod result_mod;
pub(crate) mod str_mod;
pub(crate) mod sys;
pub(crate) mod time;
pub(crate) mod uuid;
pub(crate) mod vec_mod;

/// All standard library native functions, keyed by qualified name.
pub fn stdlib_natives() -> HashMap<String, NativeEntry> {
    // Register the fused-spawn constructor (see `SpawnHook`): idempotent,
    // and every interpreter-building path calls this function, so the VM's
    // `SpawnClosure` op always finds it.
    let _ = zz_runtime::SPAWN_HOOK.get_or_init(|| concurrency::spawn_hook);
    let mut m = HashMap::new();

    // Builtin console I/O — no import required, no `std.io` module.
    m.insert(
        "print".into(),
        NativeEntry {
            arity: 1,
            f: io::print,
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
    // Test assertions — top-level builtins available without import.
    // `assert(cond)` = arity 1; `assert(cond, msg)` not supported at VM level
    // (arity is fixed for natives). The 2-arg form is via `std.test.assert`
    // which the checker allows via has_default; for simplicity we register
    // a single-arg `assert` native. Users wanting messages use `fail()` or
    // `assert_eq` for richer output.
    m.insert(
        "assert".into(),
        NativeEntry {
            arity: 1,
            f: assert::assert_fn,
        },
    );
    m.insert(
        "assert_eq".into(),
        NativeEntry {
            arity: 2,
            f: assert::assert_eq_fn,
        },
    );
    m.insert(
        "assert_ne".into(),
        NativeEntry {
            arity: 2,
            f: assert::assert_ne_fn,
        },
    );
    m.insert(
        "assert_approx_eq".into(),
        NativeEntry {
            arity: 3,
            f: assert::assert_approx_eq_fn,
        },
    );
    m.insert(
        "fail".into(),
        NativeEntry {
            arity: 1,
            f: assert::fail_fn,
        },
    );
    m.insert(
        "panic".into(),
        NativeEntry {
            arity: 1,
            f: assert::fail_fn,
        },
    );
    // `std.test` namespace — same implementations.
    for (short, arity, func) in [
        (
            "std.test.assert",
            1_usize,
            assert::assert_fn as zz_runtime::NativeFn,
        ),
        ("std.test.assert_eq", 2, assert::assert_eq_fn),
        ("std.test.assert_ne", 2, assert::assert_ne_fn),
        ("std.test.assert_approx_eq", 3, assert::assert_approx_eq_fn),
        ("std.test.fail", 1, assert::fail_fn),
        ("std.test.panic", 1, assert::fail_fn),
    ] {
        m.insert(short.into(), NativeEntry { arity, f: func });
    }

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
    // bytes.* methods (for method dispatch: `b.len()` on byte buffers).
    m.insert(
        "bytes.len".into(),
        NativeEntry {
            arity: 1,
            f: iterators::len,
        },
    );
    m.insert(
        "std.bytes.len".into(),
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
    // vec.enumerate / std.vec.enumerate — method spellings of `enumerate`.
    m.insert(
        "vec.enumerate".into(),
        NativeEntry {
            arity: 1,
            f: iterators::enumerate,
        },
    );
    m.insert(
        "std.vec.enumerate".into(),
        NativeEntry {
            arity: 1,
            f: iterators::enumerate,
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
    // json.* short-form (for pure-ZZ and import-free use)
    m.insert(
        "json.parse".into(),
        NativeEntry {
            arity: 1,
            f: json::json_parse,
        },
    );
    m.insert(
        "json.stringify".into(),
        NativeEntry {
            arity: 1,
            f: json::json_stringify,
        },
    );
    m.insert(
        "json.get".into(),
        NativeEntry {
            arity: 2,
            f: json::json_get,
        },
    );
    m.insert(
        "json.null".into(),
        NativeEntry {
            arity: 0,
            f: json::json_null,
        },
    );
    m.insert(
        "json.as_str".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_str,
        },
    );
    m.insert(
        "json.as_int".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_int,
        },
    );
    m.insert(
        "json.as_float".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_float,
        },
    );
    m.insert(
        "json.as_bool".into(),
        NativeEntry {
            arity: 1,
            f: json::json_as_bool,
        },
    );
    m.insert(
        "json.pretty".into(),
        NativeEntry {
            arity: 1,
            f: json::json_pretty,
        },
    );
    m.insert(
        "json.type".into(),
        NativeEntry {
            arity: 1,
            f: json::json_type,
        },
    );
    m.insert(
        "json.len".into(),
        NativeEntry {
            arity: 1,
            f: json::json_len,
        },
    );
    m.insert(
        "json.keys".into(),
        NativeEntry {
            arity: 1,
            f: json::json_keys,
        },
    );
    m.insert(
        "json.has".into(),
        NativeEntry {
            arity: 2,
            f: json::json_has,
        },
    );
    m.insert(
        "json.merge".into(),
        NativeEntry {
            arity: 2,
            f: json::json_merge,
        },
    );
    m.insert(
        "json.deep_get".into(),
        NativeEntry {
            arity: 2,
            f: json::json_deep_get,
        },
    );
    m.insert(
        "json.array_push".into(),
        NativeEntry {
            arity: 2,
            f: json::json_array_push,
        },
    );
    m.insert(
        "std.json.pretty".into(),
        NativeEntry {
            arity: 1,
            f: json::json_pretty,
        },
    );
    m.insert(
        "std.json.type".into(),
        NativeEntry {
            arity: 1,
            f: json::json_type,
        },
    );
    m.insert(
        "std.json.len".into(),
        NativeEntry {
            arity: 1,
            f: json::json_len,
        },
    );
    m.insert(
        "std.json.keys".into(),
        NativeEntry {
            arity: 1,
            f: json::json_keys,
        },
    );
    m.insert(
        "std.json.has".into(),
        NativeEntry {
            arity: 2,
            f: json::json_has,
        },
    );
    m.insert(
        "std.json.merge".into(),
        NativeEntry {
            arity: 2,
            f: json::json_merge,
        },
    );
    m.insert(
        "std.json.deep_get".into(),
        NativeEntry {
            arity: 2,
            f: json::json_deep_get,
        },
    );
    m.insert(
        "std.json.array_push".into(),
        NativeEntry {
            arity: 2,
            f: json::json_array_push,
        },
    );

    // std.regexp — compiled-pattern handles (`Value::Opaque`, tag "regexp").
    // Both spellings are registered (like `std.json.*` / `json.*`): the
    // `std.*` keys back `import std.regexp`, the bare keys back method
    // dispatch (`re.is_match(s)`) and direct calls.
    m.insert(
        "std.regexp.compile".into(),
        NativeEntry {
            arity: 1,
            f: regexp::regexp_compile,
        },
    );
    m.insert(
        "std.regexp.is_match".into(),
        NativeEntry {
            arity: 2,
            f: regexp::regexp_is_match,
        },
    );
    m.insert(
        "std.regexp.find".into(),
        NativeEntry {
            arity: 2,
            f: regexp::regexp_find,
        },
    );
    m.insert(
        "std.regexp.replace_all".into(),
        NativeEntry {
            arity: 3,
            f: regexp::regexp_replace_all,
        },
    );
    m.insert(
        "std.regexp.captures".into(),
        NativeEntry {
            arity: 2,
            f: regexp::regexp_captures,
        },
    );
    m.insert(
        "regexp.compile".into(),
        NativeEntry {
            arity: 1,
            f: regexp::regexp_compile,
        },
    );
    m.insert(
        "regexp.is_match".into(),
        NativeEntry {
            arity: 2,
            f: regexp::regexp_is_match,
        },
    );
    m.insert(
        "regexp.find".into(),
        NativeEntry {
            arity: 2,
            f: regexp::regexp_find,
        },
    );
    m.insert(
        "regexp.replace_all".into(),
        NativeEntry {
            arity: 3,
            f: regexp::regexp_replace_all,
        },
    );
    m.insert(
        "regexp.captures".into(),
        NativeEntry {
            arity: 2,
            f: regexp::regexp_captures,
        },
    );

    // std.crypto — digests (hex), HMAC, CSPRNG, constant-time equality.
    // Both spellings registered (like `std.json.*` / `json.*`).
    m.insert(
        "std.crypto.sha256".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_sha256,
        },
    );
    m.insert(
        "std.crypto.sha256_bytes".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_sha256_bytes,
        },
    );
    m.insert(
        "std.crypto.sha512".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_sha512,
        },
    );
    m.insert(
        "std.crypto.hmac_sha256".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_hmac_sha256,
        },
    );
    m.insert(
        "std.crypto.random_bytes".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_random_bytes,
        },
    );
    m.insert(
        "std.crypto.ct_eq".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_ct_eq,
        },
    );
    m.insert(
        "crypto.sha256".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_sha256,
        },
    );
    m.insert(
        "crypto.sha256_bytes".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_sha256_bytes,
        },
    );
    m.insert(
        "crypto.sha512".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_sha512,
        },
    );
    m.insert(
        "crypto.hmac_sha256".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_hmac_sha256,
        },
    );
    m.insert(
        "crypto.random_bytes".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_random_bytes,
        },
    );
    m.insert(
        "crypto.ct_eq".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_ct_eq,
        },
    );
    m.insert(
        "std.crypto.argon2_hash".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_argon2_hash,
        },
    );
    m.insert(
        "std.crypto.argon2_verify".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_argon2_verify,
        },
    );
    m.insert(
        "std.crypto.bcrypt_hash".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_bcrypt_hash,
        },
    );
    m.insert(
        "std.crypto.bcrypt_verify".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_bcrypt_verify,
        },
    );
    m.insert(
        "crypto.argon2_hash".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_argon2_hash,
        },
    );
    m.insert(
        "crypto.argon2_verify".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_argon2_verify,
        },
    );
    m.insert(
        "crypto.bcrypt_hash".into(),
        NativeEntry {
            arity: 1,
            f: crypto::crypto_bcrypt_hash,
        },
    );
    m.insert(
        "crypto.bcrypt_verify".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_bcrypt_verify,
        },
    );
    // Asymmetric crypto + JWT (both spellings each).
    m.insert(
        "std.crypto.ed25519_keypair".into(),
        NativeEntry {
            arity: 0,
            f: crypto::crypto_ed25519_keypair,
        },
    );
    m.insert(
        "std.crypto.ed25519_sign".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_ed25519_sign,
        },
    );
    m.insert(
        "std.crypto.ed25519_verify".into(),
        NativeEntry {
            arity: 3,
            f: crypto::crypto_ed25519_verify,
        },
    );
    m.insert(
        "std.crypto.rsa_keypair".into(),
        NativeEntry {
            arity: 0,
            f: crypto::crypto_rsa_keypair,
        },
    );
    m.insert(
        "std.crypto.rsa_sign".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_rsa_sign,
        },
    );
    m.insert(
        "std.crypto.rsa_verify".into(),
        NativeEntry {
            arity: 3,
            f: crypto::crypto_rsa_verify,
        },
    );
    m.insert(
        "std.crypto.jwt_encode".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_encode,
        },
    );
    m.insert(
        "std.crypto.jwt_decode".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_decode,
        },
    );
    m.insert(
        "std.crypto.jwt_encode_ed".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_encode_ed,
        },
    );
    m.insert(
        "std.crypto.jwt_decode_ed".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_decode_ed,
        },
    );
    m.insert(
        "crypto.ed25519_keypair".into(),
        NativeEntry {
            arity: 0,
            f: crypto::crypto_ed25519_keypair,
        },
    );
    m.insert(
        "crypto.ed25519_sign".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_ed25519_sign,
        },
    );
    m.insert(
        "crypto.ed25519_verify".into(),
        NativeEntry {
            arity: 3,
            f: crypto::crypto_ed25519_verify,
        },
    );
    m.insert(
        "crypto.rsa_keypair".into(),
        NativeEntry {
            arity: 0,
            f: crypto::crypto_rsa_keypair,
        },
    );
    m.insert(
        "crypto.rsa_sign".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_rsa_sign,
        },
    );
    m.insert(
        "crypto.rsa_verify".into(),
        NativeEntry {
            arity: 3,
            f: crypto::crypto_rsa_verify,
        },
    );
    m.insert(
        "crypto.jwt_encode".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_encode,
        },
    );
    m.insert(
        "crypto.jwt_decode".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_decode,
        },
    );
    m.insert(
        "crypto.jwt_encode_ed".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_encode_ed,
        },
    );
    m.insert(
        "crypto.jwt_decode_ed".into(),
        NativeEntry {
            arity: 2,
            f: crypto::crypto_jwt_decode_ed,
        },
    );

    // std.log — levels, sinks, spans (both spellings each).
    m.insert(
        "std.log.set_level".into(),
        NativeEntry {
            arity: 1,
            f: log::log_set_level,
        },
    );
    m.insert(
        "std.log.get_level".into(),
        NativeEntry {
            arity: 0,
            f: log::log_get_level,
        },
    );
    m.insert(
        "std.log.set_format".into(),
        NativeEntry {
            arity: 1,
            f: log::log_set_format,
        },
    );
    m.insert(
        "std.log.to_file".into(),
        NativeEntry {
            arity: 1,
            f: log::log_to_file,
        },
    );
    m.insert(
        "std.log.to_stderr".into(),
        NativeEntry {
            arity: 0,
            f: log::log_to_stderr,
        },
    );
    m.insert(
        "std.log.trace".into(),
        NativeEntry {
            arity: 1,
            f: log::log_trace,
        },
    );
    m.insert(
        "std.log.debug".into(),
        NativeEntry {
            arity: 1,
            f: log::log_debug,
        },
    );
    m.insert(
        "std.log.info".into(),
        NativeEntry {
            arity: 1,
            f: log::log_info,
        },
    );
    m.insert(
        "std.log.warn".into(),
        NativeEntry {
            arity: 1,
            f: log::log_warn,
        },
    );
    m.insert(
        "std.log.error".into(),
        NativeEntry {
            arity: 1,
            f: log::log_error,
        },
    );
    m.insert(
        "std.log.span_begin".into(),
        NativeEntry {
            arity: 1,
            f: log::log_span_begin,
        },
    );
    m.insert(
        "std.span.end".into(),
        NativeEntry {
            arity: 1,
            f: log::span_end,
        },
    );
    m.insert(
        "log.set_level".into(),
        NativeEntry {
            arity: 1,
            f: log::log_set_level,
        },
    );
    m.insert(
        "log.get_level".into(),
        NativeEntry {
            arity: 0,
            f: log::log_get_level,
        },
    );
    m.insert(
        "log.set_format".into(),
        NativeEntry {
            arity: 1,
            f: log::log_set_format,
        },
    );
    m.insert(
        "log.to_file".into(),
        NativeEntry {
            arity: 1,
            f: log::log_to_file,
        },
    );
    m.insert(
        "log.to_stderr".into(),
        NativeEntry {
            arity: 0,
            f: log::log_to_stderr,
        },
    );
    m.insert(
        "log.trace".into(),
        NativeEntry {
            arity: 1,
            f: log::log_trace,
        },
    );
    m.insert(
        "log.debug".into(),
        NativeEntry {
            arity: 1,
            f: log::log_debug,
        },
    );
    m.insert(
        "log.info".into(),
        NativeEntry {
            arity: 1,
            f: log::log_info,
        },
    );
    m.insert(
        "log.warn".into(),
        NativeEntry {
            arity: 1,
            f: log::log_warn,
        },
    );
    m.insert(
        "log.error".into(),
        NativeEntry {
            arity: 1,
            f: log::log_error,
        },
    );
    m.insert(
        "log.span_begin".into(),
        NativeEntry {
            arity: 1,
            f: log::log_span_begin,
        },
    );
    m.insert(
        "span.end".into(),
        NativeEntry {
            arity: 1,
            f: log::span_end,
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
        "std.encoding.base64_decode_bytes".into(),
        NativeEntry {
            arity: 1,
            f: encoding::encoding_base64_decode_bytes,
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
        "http.log".into(),
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
        "http.pipe".into(),
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
        "http.serve_dir".into(),
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
        "std.http.respond".into(),
        NativeEntry {
            arity: 3,
            f: http::http_respond,
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

    // std.fs — comprehensive non-blocking filesystem (see
    // `natives/fs/mod.rs` for the I/O pool + yield protocol). Legacy
    // aliases (`read_file`/`write_file`/`read`/`remove`) are kept so
    // existing fixtures keep working.
    for (name, arity, func) in [
        (
            "std.fs.read_file",
            1_usize,
            fs::fs_read_file as zz_runtime::NativeFn,
        ),
        ("std.fs.read", 1, fs::fs_read_file),
        ("std.fs.read_to_string", 1, fs::fs_read_file),
        ("std.fs.read_bytes", 1, fs::fs_read_bytes),
        ("std.fs.write_file", 2, fs::fs_write_file),
        ("std.fs.write", 2, fs::fs_write_file),
        ("std.fs.append", 2, fs::fs_append),
        ("std.fs.copy", 2, fs::fs_copy),
        ("std.fs.move", 2, fs::fs_move),
        ("std.fs.rename", 2, fs::fs_move),
        ("std.fs.exists", 1, fs::fs_exists),
        ("std.fs.is_file", 1, fs::fs_is_file),
        ("std.fs.is_dir", 1, fs::fs_is_dir),
        ("std.fs.remove_file", 1, fs::fs_remove_file),
        ("std.fs.remove", 1, fs::fs_remove_file),
        ("std.fs.mkdir", 1, fs::fs_mkdir),
        ("std.fs.mkdir_all", 1, fs::fs_mkdir_all),
        ("std.fs.read_dir", 1, fs::fs_read_dir),
        ("std.fs.readdir", 1, fs::fs_read_dir),
        ("std.fs.remove_dir_all", 1, fs::fs_remove_dir_all),
        ("std.fs.walk_dir", 1, fs::fs_walk_dir),
        ("std.fs.stat", 1, fs::fs_stat),
        ("std.fs.open", 2, fs::fs_open),
        ("std.fs.read_chunk", 2, fs::file_read_chunk),
        ("std.fs.read_chunk_bytes", 2, fs::file_read_chunk_bytes),
        ("std.fs.write_chunk", 2, fs::file_write_chunk),
        ("std.fs.seek", 2, fs::file_seek),
        ("std.fs.flush", 1, fs::file_flush),
        ("std.fs.close", 1, fs::file_close),
        // Pure cross-platform path lexing (no I/O; see `natives/fs/path.rs`).
        ("std.fs.normalize", 1, fs::fs_normalize),
        ("std.fs.join", 2, fs::fs_join),
        ("std.fs.basename", 1, fs::fs_basename),
        ("std.fs.dirname", 1, fs::fs_dirname),
        ("std.fs.is_absolute", 1, fs::fs_is_absolute),
        ("std.fs.extension", 1, fs::fs_extension),
        // FS providers (`fs.FS` handle interface: Os / Mem / Tar / Embed)
        // plus the `*_at` operation family (see `natives/fs/vfs.rs).
        ("std.fs.osfs", 0, fs::vfs::fs_osfs),
        ("std.fs.memfs", 0, fs::vfs::fs_memfs),
        ("std.fs.tarfs", 1, fs::vfs::fs_tarfs),
        ("std.fs.embedfs", 0, fs::vfs::fs_embedfs),
        ("std.fs.read_to_string_at", 2, fs::vfs::fs_read_to_string_at),
        ("std.fs.read_bytes_at", 2, fs::vfs::fs_read_bytes_at),
        ("std.fs.write_at", 3, fs::vfs::fs_write_at),
        ("std.fs.append_at", 3, fs::vfs::fs_append_at),
        ("std.fs.exists_at", 2, fs::vfs::fs_exists_at),
        ("std.fs.is_file_at", 2, fs::vfs::fs_is_file_at),
        ("std.fs.is_dir_at", 2, fs::vfs::fs_is_dir_at),
        ("std.fs.read_dir_at", 2, fs::vfs::fs_read_dir_at),
        ("std.fs.mkdir_all_at", 2, fs::vfs::fs_mkdir_all_at),
        ("std.fs.remove_file_at", 2, fs::vfs::fs_remove_file_at),
        // `File.open` constructor namespace (mirrors `Regexp.new`).
        ("File.open", 2, fs::fs_open),
        // `file.*` method namespace for open-handle dispatch.
        ("file.read_chunk", 2, fs::file_read_chunk),
        ("file.read_chunk_bytes", 2, fs::file_read_chunk_bytes),
        ("file.write_chunk", 2, fs::file_write_chunk),
        ("file.seek", 2, fs::file_seek),
        ("file.flush", 1, fs::file_flush),
        ("file.close", 1, fs::file_close),
    ] {
        m.insert(name.into(), NativeEntry { arity, f: func });
    }

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
    // std.env — cross-platform environment + OS identity (see
    // `natives/env/mod.rs`). Short `env.*` spellings resolve through the
    // module namespace like every other stdlib module.
    for (name, arity, func) in [
        ("std.env.get", 1_usize, env::env_get as zz_runtime::NativeFn),
        ("std.env.set", 2, env::env_set),
        ("std.env.remove", 1, env::env_remove),
        ("std.env.unset", 1, env::env_remove),
        ("std.env.vars", 0, env::env_vars),
        ("std.env.cwd", 0, env::env_cwd),
        ("std.env.set_cwd", 1, env::env_set_cwd),
        ("std.env.exe_path", 0, env::env_exe_path),
        ("std.env.home_dir", 0, env::env_home_dir),
        ("std.env.temp_dir", 0, env::env_temp_dir),
        ("std.env.user", 0, env::env_user),
        ("std.env.os", 0, env::env_os),
        ("std.env.arch", 0, env::env_arch),
    ] {
        m.insert(name.into(), NativeEntry { arity, f: func });
    }

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
    m.insert(
        "std.time.now_nanos".into(),
        NativeEntry {
            arity: 0,
            f: time::time_now_nanos,
        },
    );
    m.insert(
        "std.time.now_micros".into(),
        NativeEntry {
            arity: 0,
            f: time::time_now_micros,
        },
    );
    m.insert(
        "std.time.monotonic_nanos".into(),
        NativeEntry {
            arity: 0,
            f: time::time_monotonic_nanos,
        },
    );
    m.insert(
        "std.time.sleep_micros".into(),
        NativeEntry {
            arity: 1,
            f: time::time_sleep_micros,
        },
    );
    // Bare `time.*` aliases (mirror the `std.time.*` natives).
    m.insert(
        "time.now_ms".into(),
        NativeEntry {
            arity: 0,
            f: time::time_now_ms,
        },
    );
    m.insert(
        "time.sleep_ms".into(),
        NativeEntry {
            arity: 1,
            f: time::time_sleep_ms,
        },
    );
    m.insert(
        "time.now_nanos".into(),
        NativeEntry {
            arity: 0,
            f: time::time_now_nanos,
        },
    );
    m.insert(
        "time.now_micros".into(),
        NativeEntry {
            arity: 0,
            f: time::time_now_micros,
        },
    );
    m.insert(
        "time.monotonic_nanos".into(),
        NativeEntry {
            arity: 0,
            f: time::time_monotonic_nanos,
        },
    );
    m.insert(
        "time.sleep_micros".into(),
        NativeEntry {
            arity: 1,
            f: time::time_sleep_micros,
        },
    );

    // std.sys — system information (both spellings each).
    m.insert(
        "std.sys.os".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_os,
        },
    );
    m.insert(
        "std.sys.arch".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_arch,
        },
    );
    m.insert(
        "std.sys.cpu_count".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_cpu_count,
        },
    );
    m.insert(
        "std.sys.hostname".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_hostname,
        },
    );
    m.insert(
        "std.sys.total_mem".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_total_mem,
        },
    );
    m.insert(
        "std.sys.avail_mem".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_avail_mem,
        },
    );
    m.insert(
        "sys.os".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_os,
        },
    );
    m.insert(
        "sys.arch".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_arch,
        },
    );
    m.insert(
        "sys.cpu_count".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_cpu_count,
        },
    );
    m.insert(
        "sys.hostname".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_hostname,
        },
    );
    m.insert(
        "sys.total_mem".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_total_mem,
        },
    );
    m.insert(
        "sys.avail_mem".into(),
        NativeEntry {
            arity: 0,
            f: sys::sys_avail_mem,
        },
    );

    // std.args — raw argv + flag parser (both spellings each).
    m.insert(
        "std.args.get_raw".into(),
        NativeEntry {
            arity: 0,
            f: args::args_get_raw,
        },
    );
    m.insert(
        "std.args.parser".into(),
        NativeEntry {
            arity: 0,
            f: args::args_parser,
        },
    );
    m.insert(
        "std.args.str_flag".into(),
        NativeEntry {
            arity: 3,
            f: args::args_str_flag,
        },
    );
    m.insert(
        "std.args.int_flag".into(),
        NativeEntry {
            arity: 3,
            f: args::args_int_flag,
        },
    );
    m.insert(
        "std.args.bool_flag".into(),
        NativeEntry {
            arity: 2,
            f: args::args_bool_flag,
        },
    );
    m.insert(
        "std.args.parse".into(),
        NativeEntry {
            arity: 2,
            f: args::args_parse,
        },
    );
    m.insert(
        "std.args.get_str".into(),
        NativeEntry {
            arity: 2,
            f: args::args_get_str,
        },
    );
    m.insert(
        "std.args.get_int".into(),
        NativeEntry {
            arity: 2,
            f: args::args_get_int,
        },
    );
    m.insert(
        "std.args.get_bool".into(),
        NativeEntry {
            arity: 2,
            f: args::args_get_bool,
        },
    );
    m.insert(
        "std.args.positional".into(),
        NativeEntry {
            arity: 2,
            f: args::args_positional,
        },
    );
    m.insert(
        "std.args.subcommand".into(),
        NativeEntry {
            arity: 1,
            f: args::args_subcommand,
        },
    );
    m.insert(
        "std.args.help".into(),
        NativeEntry {
            arity: 2,
            f: args::args_help,
        },
    );
    m.insert(
        "std.args.error".into(),
        NativeEntry {
            arity: 1,
            f: args::args_error,
        },
    );
    m.insert(
        "std.args.was_help".into(),
        NativeEntry {
            arity: 1,
            f: args::args_was_help,
        },
    );
    m.insert(
        "args.get_raw".into(),
        NativeEntry {
            arity: 0,
            f: args::args_get_raw,
        },
    );
    m.insert(
        "args.parser".into(),
        NativeEntry {
            arity: 0,
            f: args::args_parser,
        },
    );
    m.insert(
        "args.str_flag".into(),
        NativeEntry {
            arity: 3,
            f: args::args_str_flag,
        },
    );
    m.insert(
        "args.int_flag".into(),
        NativeEntry {
            arity: 3,
            f: args::args_int_flag,
        },
    );
    m.insert(
        "args.bool_flag".into(),
        NativeEntry {
            arity: 2,
            f: args::args_bool_flag,
        },
    );
    m.insert(
        "args.parse".into(),
        NativeEntry {
            arity: 2,
            f: args::args_parse,
        },
    );
    m.insert(
        "args.get_str".into(),
        NativeEntry {
            arity: 2,
            f: args::args_get_str,
        },
    );
    m.insert(
        "args.get_int".into(),
        NativeEntry {
            arity: 2,
            f: args::args_get_int,
        },
    );
    m.insert(
        "args.get_bool".into(),
        NativeEntry {
            arity: 2,
            f: args::args_get_bool,
        },
    );
    m.insert(
        "args.positional".into(),
        NativeEntry {
            arity: 2,
            f: args::args_positional,
        },
    );
    m.insert(
        "args.subcommand".into(),
        NativeEntry {
            arity: 1,
            f: args::args_subcommand,
        },
    );
    m.insert(
        "args.help".into(),
        NativeEntry {
            arity: 2,
            f: args::args_help,
        },
    );
    m.insert(
        "args.error".into(),
        NativeEntry {
            arity: 1,
            f: args::args_error,
        },
    );
    m.insert(
        "args.was_help".into(),
        NativeEntry {
            arity: 1,
            f: args::args_was_help,
        },
    );

    // std.process — run/spawn/wait/exit/pid (both spellings each).
    m.insert(
        "std.process.run".into(),
        NativeEntry {
            arity: 2,
            f: process::process_run,
        },
    );
    m.insert(
        "std.process.run_with_env".into(),
        NativeEntry {
            arity: 3,
            f: process::process_run_with_env,
        },
    );
    m.insert(
        "std.process.spawn".into(),
        NativeEntry {
            arity: 2,
            f: process::process_spawn,
        },
    );
    m.insert(
        "std.process.wait".into(),
        NativeEntry {
            arity: 1,
            f: process::process_wait,
        },
    );
    m.insert(
        "std.process.exit".into(),
        NativeEntry {
            arity: 1,
            f: process::process_exit,
        },
    );
    m.insert(
        "std.process.pid".into(),
        NativeEntry {
            arity: 0,
            f: process::process_pid,
        },
    );
    m.insert(
        "process.run".into(),
        NativeEntry {
            arity: 2,
            f: process::process_run,
        },
    );
    m.insert(
        "process.run_with_env".into(),
        NativeEntry {
            arity: 3,
            f: process::process_run_with_env,
        },
    );
    m.insert(
        "process.spawn".into(),
        NativeEntry {
            arity: 2,
            f: process::process_spawn,
        },
    );
    m.insert(
        "process.wait".into(),
        NativeEntry {
            arity: 1,
            f: process::process_wait,
        },
    );
    m.insert(
        "process.exit".into(),
        NativeEntry {
            arity: 1,
            f: process::process_exit,
        },
    );
    m.insert(
        "process.pid".into(),
        NativeEntry {
            arity: 0,
            f: process::process_pid,
        },
    );

    // std.uuid — v4/v7 generation, parse, validation (both spellings).
    m.insert(
        "std.uuid.v4".into(),
        NativeEntry {
            arity: 0,
            f: uuid::uuid_v4,
        },
    );
    m.insert(
        "std.uuid.v7".into(),
        NativeEntry {
            arity: 0,
            f: uuid::uuid_v7,
        },
    );
    m.insert(
        "std.uuid.parse".into(),
        NativeEntry {
            arity: 1,
            f: uuid::uuid_parse,
        },
    );
    m.insert(
        "std.uuid.is_valid".into(),
        NativeEntry {
            arity: 1,
            f: uuid::uuid_is_valid,
        },
    );
    m.insert(
        "uuid.v4".into(),
        NativeEntry {
            arity: 0,
            f: uuid::uuid_v4,
        },
    );
    m.insert(
        "uuid.v7".into(),
        NativeEntry {
            arity: 0,
            f: uuid::uuid_v7,
        },
    );
    m.insert(
        "uuid.parse".into(),
        NativeEntry {
            arity: 1,
            f: uuid::uuid_parse,
        },
    );
    m.insert(
        "uuid.is_valid".into(),
        NativeEntry {
            arity: 1,
            f: uuid::uuid_is_valid,
        },
    );

    // std.chan — concurrency primitives
    m.insert(
        "std.chan".into(),
        NativeEntry {
            arity: 0,
            f: concurrency::chan_new,
        },
    );
    m.insert(
        "std.chan.send".into(),
        NativeEntry {
            arity: 2,
            f: concurrency::chan_send,
        },
    );
    m.insert(
        "std.chan.recv".into(),
        NativeEntry {
            arity: 1,
            f: concurrency::chan_recv,
        },
    );
    m.insert(
        "std.chan.try_recv".into(),
        NativeEntry {
            arity: 1,
            f: concurrency::chan_try_recv,
        },
    );

    // std.task — spawn and join
    m.insert(
        "std.task.spawn".into(),
        NativeEntry {
            arity: 1,
            f: concurrency::spawn,
        },
    );
    m.insert(
        "std.task.join".into(),
        NativeEntry {
            arity: 1,
            f: concurrency::task_join,
        },
    );
    m.insert(
        "std.task.try_join".into(),
        NativeEntry {
            arity: 1,
            f: concurrency::task_try_join,
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

    // std.sqlz — SQLite (rusqlite bundled), CANONICAL module name.
    // `query`/`exec` take (db, template, ...bound_params); the VM's DbQuery
    // op splits the interpolated SQL into template + params so values are
    // bound via prepared statements, never concatenated.
    // `std.db` / `db.*` below are zero-overhead aliases (same fn pointers).
    m.insert(
        "std.sqlz.open".into(),
        NativeEntry {
            arity: 1,
            f: db::db_open,
        },
    );
    m.insert(
        "std.sqlz.exec".into(),
        NativeEntry {
            arity: 3,
            f: db::db_exec,
        },
    );
    m.insert(
        "std.sqlz.query".into(),
        NativeEntry {
            arity: 3,
            f: db::db_query,
        },
    );
    m.insert(
        "std.sqlz.close".into(),
        NativeEntry {
            arity: 1,
            f: db::db_close,
        },
    );
    // Method-dispatch aliases (`sqlz.open(...)` after `import std.sqlz`).
    m.insert(
        "sqlz.open".into(),
        NativeEntry {
            arity: 1,
            f: db::db_open,
        },
    );
    m.insert(
        "sqlz.exec".into(),
        NativeEntry {
            arity: 3,
            f: db::db_exec,
        },
    );
    m.insert(
        "sqlz.query".into(),
        NativeEntry {
            arity: 3,
            f: db::db_query,
        },
    );
    m.insert(
        "sqlz.close".into(),
        NativeEntry {
            arity: 1,
            f: db::db_close,
        },
    );
    // Alias: `std.db` / `db.*` point directly at the `std.sqlz` impls.
    m.insert(
        "std.db.open".into(),
        NativeEntry {
            arity: 1,
            f: db::db_open,
        },
    );
    m.insert(
        "std.db.exec".into(),
        NativeEntry {
            arity: 3,
            f: db::db_exec,
        },
    );
    m.insert(
        "std.db.query".into(),
        NativeEntry {
            arity: 3,
            f: db::db_query,
        },
    );
    m.insert(
        "std.db.close".into(),
        NativeEntry {
            arity: 1,
            f: db::db_close,
        },
    );
    // Method-dispatch aliases (`db.open(...)` after `import std.db`).
    m.insert(
        "db.open".into(),
        NativeEntry {
            arity: 1,
            f: db::db_open,
        },
    );
    m.insert(
        "db.exec".into(),
        NativeEntry {
            arity: 3,
            f: db::db_exec,
        },
    );
    m.insert(
        "db.query".into(),
        NativeEntry {
            arity: 3,
            f: db::db_query,
        },
    );
    m.insert(
        "db.close".into(),
        NativeEntry {
            arity: 1,
            f: db::db_close,
        },
    );

    // std.sqlz.postgres — wire-protocol driver. Import as
    // `import std.sqlz.postgres as pg`; the namespace copies below happen
    // per-import via `register_module_namespace`, so only the canonical
    // `std.sqlz.postgres.*` keys live here (no bare pre-registration:
    // these are free functions, not method-dispatch targets).
    m.insert(
        "std.sqlz.postgres.connect".into(),
        NativeEntry {
            arity: 1,
            f: db::pg_connect,
        },
    );
    m.insert(
        "std.sqlz.postgres.exec".into(),
        NativeEntry {
            arity: 3,
            f: db::pg_exec,
        },
    );
    m.insert(
        "std.sqlz.postgres.query".into(),
        NativeEntry {
            arity: 3,
            f: db::pg_query,
        },
    );
    m.insert(
        "std.sqlz.postgres.close".into(),
        NativeEntry {
            arity: 1,
            f: db::pg_close,
        },
    );

    // std.sqlz.mysql — wire-protocol driver. Same per-import namespace
    // rule as postgres: only canonical `std.sqlz.mysql.*` keys here.
    m.insert(
        "std.sqlz.mysql.connect".into(),
        NativeEntry {
            arity: 1,
            f: db::my_connect,
        },
    );
    m.insert(
        "std.sqlz.mysql.exec".into(),
        NativeEntry {
            arity: 3,
            f: db::my_exec,
        },
    );
    m.insert(
        "std.sqlz.mysql.query".into(),
        NativeEntry {
            arity: 3,
            f: db::my_query,
        },
    );
    m.insert(
        "std.sqlz.mysql.close".into(),
        NativeEntry {
            arity: 1,
            f: db::my_close,
        },
    );

    // sqlz.transaction — closure transactions (method form works on any
    // backend handle; the free-function form takes the handle explicitly).
    m.insert(
        "std.sqlz.transaction".into(),
        NativeEntry {
            arity: 2,
            f: db::db_transaction,
        },
    );
    m.insert(
        "sqlz.transaction".into(),
        NativeEntry {
            arity: 2,
            f: db::db_transaction,
        },
    );
    m.insert(
        "std.db.transaction".into(),
        NativeEntry {
            arity: 2,
            f: db::db_transaction,
        },
    );
    m.insert(
        "db.transaction".into(),
        NativeEntry {
            arity: 2,
            f: db::db_transaction,
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

    // Built-in: `dbg(v)` — debug print preserving Option wrappers,
    // returns `v` unchanged. The only display path that keeps
    // `.some(v)` / `.none`; `println`, interpolation, and `str()`
    // all unwrap.
    m.insert(
        "dbg".into(),
        NativeEntry {
            arity: 1,
            f: builtins::dbg_fn,
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
        Value::Str(s) => Ok((**s).clone()),
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
        Value::Array(vs) => Ok((**vs).clone()),
        other => Err(EvalError::new(
            format!("`{name}` expects an array, found `{other}`"),
            zz_runtime::Span::new(0, 0),
        )),
    }
}

pub(crate) fn expect_func(args: &mut Vec<Value>, i: usize, name: &str) -> Result<Value, EvalError> {
    match arg(args, i, name)? {
        Value::Func(f) => Ok(Value::Func(Box::new((**f).clone()))),
        Value::Native(n) => Ok(Value::Native(Box::new((**n).clone()))),
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
