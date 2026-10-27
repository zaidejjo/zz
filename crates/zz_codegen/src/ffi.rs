//! Link layer for the unified Rust native runtime (Phase 0: FFI foundation).
//!
//! Execution-model contract:
//! - the VM (`zz run`) calls Rust natives directly through the interpreter's
//!   native registry — no linking involved;
//! - AOT (`zz run --native`, `zz build`) links `libzz_native_rt.a` (built
//!   from the `zz_native_rt` leaf crate) and calls its `extern "C"` `zz_rt_*`
//!   functions. Per-module native symbols (regex, crypto, …) join the same
//!   static library from Phase 1 on.
//!
//! Generated C references FFI symbols through two helpers:
//! - [`ffi_impl`] maps a zz native name (`std.regex.compile`) to its C
//!   symbol (`zz_regex_compile`); the call-emission and dispatch logic falls
//!   back to it when the embedded C runtime has no implementation;
//! - [`ffi_prelude`] emits the `extern` declarations for every used FFI
//!   symbol, injected into the generated translation unit by the lowerer.
//!
//! [`ensure_staticlib`] builds (incrementally — plain `cargo build`, whose
//! own cache makes repeat invocations cheap) and publishes an immutable
//! snapshot of the archive that links resolve against (the cargo output is
//! rewritten in place and must never be handed to a linker); [`link_args`]
//! returns the extra `cc` flags. Both are shared by `zz build` and by the
//! link test below, so the test exercises the real link path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// FFI protocol version. Re-exported from `zz_native_rt` (single source of
/// truth); AOT binaries built against a mismatched static library must be
/// rebuilt.
pub use zz_native_rt::FFI_VERSION;

/// C header for the native-runtime FFI: opaque-handle primitives shared by
/// every future module. Handles cross the boundary as `uint64_t` ids (0 is
/// invalid); tags cross as `(pointer, length)` byte strings.
pub const FFI_H: &str = r#"
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
uint64_t zz_rt_version(void);
uint64_t zz_rt_handle_alloc(const uint8_t *tag, size_t tag_len);
bool zz_rt_handle_drop(uint64_t id);
uint64_t zz_rt_handle_live(void);
bool zz_rt_handle_tag_eq(uint64_t id, const uint8_t *tag, size_t tag_len);
"#;

/// Map a zz native qualified name to its Rust-staticlib C symbol.
///
/// Populated per module as it lands (Phase 1: `std.regexp`). Both the
/// `std.<mod>.*` and bare `<mod>.*` spellings map to the same symbol,
/// mirroring the `native_impl` convention for embedded-C natives.
pub fn ffi_impl(name: &str) -> Option<&'static str> {
    match name {
        "regexp.compile" | "std.regexp.compile" => Some("zz_regexp_compile"),
        "encoding.base64_decode_bytes" | "std.encoding.base64_decode_bytes" => {
            Some("zz_encoding_base64_decode_bytes")
        }
        "regexp.is_match" | "std.regexp.is_match" => Some("zz_regexp_is_match"),
        "regexp.find" | "std.regexp.find" => Some("zz_regexp_find"),
        "regexp.replace_all" | "std.regexp.replace_all" => Some("zz_regexp_replace_all"),
        "regexp.captures" | "std.regexp.captures" => Some("zz_regexp_captures"),
        "crypto.sha256" | "std.crypto.sha256" => Some("zz_crypto_sha256"),
        "crypto.sha256_bytes" | "std.crypto.sha256_bytes" => Some("zz_crypto_sha256_bytes"),
        "crypto.sha512" | "std.crypto.sha512" => Some("zz_crypto_sha512"),
        "crypto.hmac_sha256" | "std.crypto.hmac_sha256" => Some("zz_crypto_hmac_sha256"),
        "crypto.random_bytes" | "std.crypto.random_bytes" => Some("zz_crypto_random_bytes"),
        "crypto.ct_eq" | "std.crypto.ct_eq" => Some("zz_crypto_ct_eq"),
        "crypto.argon2_hash" | "std.crypto.argon2_hash" => Some("zz_crypto_argon2_hash"),
        "crypto.argon2_verify" | "std.crypto.argon2_verify" => Some("zz_crypto_argon2_verify"),
        "crypto.bcrypt_hash" | "std.crypto.bcrypt_hash" => Some("zz_crypto_bcrypt_hash"),
        "crypto.bcrypt_verify" | "std.crypto.bcrypt_verify" => Some("zz_crypto_bcrypt_verify"),
        "crypto.ed25519_keypair" | "std.crypto.ed25519_keypair" => {
            Some("zz_crypto_ed25519_keypair")
        }
        "crypto.ed25519_sign" | "std.crypto.ed25519_sign" => Some("zz_crypto_ed25519_sign"),
        "crypto.ed25519_verify" | "std.crypto.ed25519_verify" => Some("zz_crypto_ed25519_verify"),
        "crypto.rsa_keypair" | "std.crypto.rsa_keypair" => Some("zz_crypto_rsa_keypair"),
        "crypto.rsa_sign" | "std.crypto.rsa_sign" => Some("zz_crypto_rsa_sign"),
        "crypto.rsa_verify" | "std.crypto.rsa_verify" => Some("zz_crypto_rsa_verify"),
        "crypto.jwt_encode" | "std.crypto.jwt_encode" => Some("zz_crypto_jwt_encode"),
        "crypto.jwt_decode" | "std.crypto.jwt_decode" => Some("zz_crypto_jwt_decode"),
        "crypto.jwt_encode_ed" | "std.crypto.jwt_encode_ed" => Some("zz_crypto_jwt_encode_ed"),
        "crypto.jwt_decode_ed" | "std.crypto.jwt_decode_ed" => Some("zz_crypto_jwt_decode_ed"),
        "time.now_nanos" | "std.time.now_nanos" => Some("zz_time_now_nanos"),
        "time.now_micros" | "std.time.now_micros" => Some("zz_time_now_micros"),
        "time.monotonic_nanos" | "std.time.monotonic_nanos" => Some("zz_time_monotonic_nanos"),
        "time.sleep_micros" | "std.time.sleep_micros" => Some("zz_time_sleep_micros"),
        "log.set_level" | "std.log.set_level" => Some("zz_log_set_level"),
        "log.get_level" | "std.log.get_level" => Some("zz_log_get_level"),
        "log.set_format" | "std.log.set_format" => Some("zz_log_set_format"),
        "log.to_file" | "std.log.to_file" => Some("zz_log_to_file"),
        "log.to_stderr" | "std.log.to_stderr" => Some("zz_log_to_stderr"),
        "log.trace" | "std.log.trace" => Some("zz_log_trace"),
        "log.debug" | "std.log.debug" => Some("zz_log_debug"),
        "log.info" | "std.log.info" => Some("zz_log_info"),
        "log.warn" | "std.log.warn" => Some("zz_log_warn"),
        "log.error" | "std.log.error" => Some("zz_log_error"),
        "log.span_begin" | "std.log.span_begin" => Some("zz_log_span_begin"),
        "span.end" | "std.span.end" => Some("zz_span_end"),
        "sys.os" | "std.sys.os" => Some("zz_sys_os"),
        "sys.arch" | "std.sys.arch" => Some("zz_sys_arch"),
        "sys.cpu_count" | "std.sys.cpu_count" => Some("zz_sys_cpu_count"),
        "sys.hostname" | "std.sys.hostname" => Some("zz_sys_hostname"),
        "sys.total_mem" | "std.sys.total_mem" => Some("zz_sys_total_mem"),
        "sys.avail_mem" | "std.sys.avail_mem" => Some("zz_sys_avail_mem"),
        "term.enable_raw" | "std.term.enable_raw" => Some("zz_term_enable_raw"),
        "term.disable_raw" | "std.term.disable_raw" => Some("zz_term_disable_raw"),
        "term.read_key" | "std.term.read_key" => Some("zz_term_read_key"),
        "term.poll" | "std.term.poll" => Some("zz_term_poll"),
        "term.get_size" | "std.term.get_size" => Some("zz_term_get_size"),
        "term.is_tty" | "std.term.is_tty" => Some("zz_term_is_tty"),
        "term.flush" | "std.term.flush" => Some("zz_term_flush"),
        // Raw argv shares the fixed `zz_env_args` symbol (one source of
        // truth for both spellings and both engines).
        "args.get_raw" | "std.args.get_raw" => Some("zz_env_args"),
        "args.parser" | "std.args.parser" => Some("zz_args_parser"),
        "args.str_flag" | "std.args.str_flag" => Some("zz_args_str_flag"),
        "args.int_flag" | "std.args.int_flag" => Some("zz_args_int_flag"),
        "args.bool_flag" | "std.args.bool_flag" => Some("zz_args_bool_flag"),
        "args.parse" | "std.args.parse" => Some("zz_args_parse"),
        "args.get_str" | "std.args.get_str" => Some("zz_args_get_str"),
        "args.get_int" | "std.args.get_int" => Some("zz_args_get_int"),
        "args.get_bool" | "std.args.get_bool" => Some("zz_args_get_bool"),
        "args.positional" | "std.args.positional" => Some("zz_args_positional"),
        "args.subcommand" | "std.args.subcommand" => Some("zz_args_subcommand"),
        "args.help" | "std.args.help" => Some("zz_args_help"),
        "args.error" | "std.args.error" => Some("zz_args_error"),
        "args.was_help" | "std.args.was_help" => Some("zz_args_was_help"),
        "process.run" | "std.process.run" => Some("zz_process_run"),
        "process.run_with_env" | "std.process.run_with_env" => Some("zz_process_run_with_env"),
        "process.spawn" | "std.process.spawn" => Some("zz_process_spawn"),
        "process.wait" | "std.process.wait" => Some("zz_process_wait"),
        "process.exit" | "std.process.exit" => Some("zz_process_exit"),
        "process.pid" | "std.process.pid" => Some("zz_process_pid"),
        "uuid.v4" | "std.uuid.v4" => Some("zz_uuid_v4"),
        "uuid.v7" | "std.uuid.v7" => Some("zz_uuid_v7"),
        "uuid.parse" | "std.uuid.parse" => Some("zz_uuid_parse"),
        "uuid.is_valid" | "std.uuid.is_valid" => Some("zz_uuid_is_valid"),
        "net.tls_connect" | "std.net.tls_connect" | "tls.connect" | "std.tls.connect" => {
            Some("zz_tls_connect")
        }
        "net.tls_connect_with_ca"
        | "std.net.tls_connect_with_ca"
        | "tls.connect_with_ca"
        | "std.tls.connect_with_ca" => Some("zz_tls_connect_with_ca"),
        "net.tls_connect_insecure"
        | "std.net.tls_connect_insecure"
        | "tls.connect_insecure"
        | "std.tls.connect_insecure" => Some("zz_tls_connect_insecure"),
        "net.tls_write" | "std.net.tls_write" | "tls.write" | "std.tls.write" => {
            Some("zz_tls_write")
        }
        "net.tls_write_bytes"
        | "std.net.tls_write_bytes"
        | "tls.write_bytes"
        | "std.tls.write_bytes" => Some("zz_tls_write_bytes"),
        "net.tls_read" | "std.net.tls_read" | "tls.read" | "std.tls.read" => Some("zz_tls_read"),
        "net.tls_read_bytes"
        | "std.net.tls_read_bytes"
        | "tls.read_bytes"
        | "std.tls.read_bytes" => Some("zz_tls_read_bytes"),
        "net.tls_read_exact"
        | "std.net.tls_read_exact"
        | "tls.read_exact"
        | "std.tls.read_exact" => Some("zz_tls_read_exact"),
        "net.tls_readline" | "std.net.tls_readline" | "tls.read_line" | "std.tls.read_line" => {
            Some("zz_tls_read_line")
        }
        "net.tls_set_read_timeout"
        | "std.net.tls_set_read_timeout"
        | "tls.set_read_timeout"
        | "std.tls.set_read_timeout" => Some("zz_tls_set_read_timeout"),
        "net.tls_set_write_timeout"
        | "std.net.tls_set_write_timeout"
        | "tls.set_write_timeout"
        | "std.tls.set_write_timeout" => Some("zz_tls_set_write_timeout"),
        "net.tls_close"
        | "std.net.tls_close"
        | "tls.close"
        | "std.tls.close"
        | "tls.shutdown"
        | "std.tls.shutdown"
        | "net.tls_shutdown"
        | "std.net.tls_shutdown" => Some("zz_tls_close"),
        "net.unix_connect" | "std.net.unix_connect" => Some("zz_unix_connect"),
        "net.unix_listen" | "std.net.unix_listen" => Some("zz_unix_listen"),
        "net.unix_accept"
        | "std.net.unix_accept"
        | "unix_listener.accept"
        | "std.unix_listener.accept" => Some("zz_unix_accept"),
        "net.unix_listener_close"
        | "std.net.unix_listener_close"
        | "unix_listener.close"
        | "std.unix_listener.close" => Some("zz_unix_listener_close"),
        "net.unix_write" | "std.net.unix_write" | "unix.write" | "std.unix.write" => {
            Some("zz_unix_write")
        }
        "net.unix_write_bytes"
        | "std.net.unix_write_bytes"
        | "unix.write_bytes"
        | "std.unix.write_bytes" => Some("zz_unix_write_bytes"),
        "net.unix_read" | "std.net.unix_read" | "unix.read" | "std.unix.read" => {
            Some("zz_unix_read")
        }
        "net.unix_read_bytes"
        | "std.net.unix_read_bytes"
        | "unix.read_bytes"
        | "std.unix.read_bytes" => Some("zz_unix_read_bytes"),
        "net.unix_read_exact"
        | "std.net.unix_read_exact"
        | "unix.read_exact"
        | "std.unix.read_exact" => Some("zz_unix_read_exact"),
        "net.unix_readline" | "std.net.unix_readline" | "unix.read_line" | "std.unix.read_line" => {
            Some("zz_unix_read_line")
        }
        "net.unix_set_read_timeout"
        | "std.net.unix_set_read_timeout"
        | "unix.set_read_timeout"
        | "std.unix.set_read_timeout" => Some("zz_unix_set_read_timeout"),
        "net.unix_set_write_timeout"
        | "std.net.unix_set_write_timeout"
        | "unix.set_write_timeout"
        | "std.unix.set_write_timeout" => Some("zz_unix_set_write_timeout"),
        "net.unix_close"
        | "std.net.unix_close"
        | "unix.close"
        | "std.unix.close"
        | "unix.shutdown"
        | "std.unix.shutdown"
        | "net.unix_shutdown"
        | "std.net.unix_shutdown" => Some("zz_unix_close"),
        "net.tls_listen" | "std.net.tls_listen" => Some("zz_tls_listen"),
        "net.tls_accept"
        | "std.net.tls_accept"
        | "tls_listener.accept"
        | "std.tls_listener.accept" => Some("zz_tls_accept"),
        "net.tls_listener_close"
        | "std.net.tls_listener_close"
        | "tls_listener.close"
        | "std.tls_listener.close" => Some("zz_tls_listener_close"),
        "net.tls_peer_addr" | "std.net.tls_peer_addr" | "tls.peer_addr" | "std.tls.peer_addr" => {
            Some("zz_tls_peer_addr")
        }
        _ => None,
    }
}

/// True when any reachable native is provided by the Rust static library
/// rather than the embedded C runtime. The build links `libzz_native_rt.a`
/// exactly in that case (plus an explicit opt-in via
/// [`crate::BuildOptions::native_rt`]).
///
/// Note: `args.get_raw` maps to the embedded `zz_env_args` symbol, so it
/// alone never triggers the staticlib link (keeps `--static` working).
///
/// SQL note: the C `zz_db_*` natives reference the Postgres symbols
/// unconditionally (backend dispatch is runtime), so ANY sqlz/db/pg
/// reachability links the staticlib — even for sqlite-only programs.
/// The linker pulls only referenced objects and untouched pages never
/// fault in, so the cost is link time + file size, never RSS; registry
/// sources open PG URLs from env vars, which no static analysis can
/// prove, so the conservative gate is load-bearing, not lazy.
pub fn needs_native_rt(natives: &HashSet<String>) -> bool {
    natives.iter().any(|n| {
        if is_sql_native(n) {
            return true;
        }
        matches!(ffi_impl(n), Some(s) if s != "zz_env_args")
    })
}

/// `sqlz.*` / `pg.*` spellings (both bare and `std.`-qualified):
/// all lower to the C handle natives whose PG backend lives in the
/// staticlib. MySQL spellings are excluded (still VM-only, lowered to
/// unit — no staticlib symbols involved).
fn is_sql_native(name: &str) -> bool {
    const PREFIXES: &[&str] = &["sqlz.", "std.sqlz.", "pg.", "std.sqlz.postgres."];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// True when the program can reach the Postgres backend: force-extract
/// its objects from the archive at link time (see `PG_LINK_SYMBOLS`).
/// Split from [`needs_native_rt`] (which only decides *whether* to link)
/// because weak C refs never pull archive members on their own.
pub fn needs_pg_link(natives: &HashSet<String>) -> bool {
    natives.iter().any(|n| is_sql_native(n))
}

/// True when reachable natives lower to C calls backed by libcurl
/// (`zz_http_get/post/fetch/post_json` → `http_client_perform`).
/// Only the outbound client needs curl; the server/route/test/dispatch
/// surface is plain epoll + libc. `fetch_insecure` is included
/// conservatively (currently VM-only, but a future C impl would also
/// ride `http_client_perform`).
pub fn needs_curl_link(natives: &HashSet<String>) -> bool {
    natives.iter().any(|n| is_http_client_native(n))
}

/// ZZ spellings that lower to curl-backed C client calls, bare and
/// `std.`-qualified. `http.post` is a prefix of `http.post_json`, so one
/// entry covers both; listed explicitly for readability.
fn is_http_client_native(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "http.get",
        "http.post",
        "http.fetch",
        "std.http.get",
        "std.http.post",
        "std.http.fetch",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// True when reachable natives lower to C calls backed by libsqlite3
/// (`zz_db_*` → `sqlite3_*`). Any sqlz/db/pg reachability counts: the C
/// handle dispatch references sqlite unconditionally (backend is chosen
/// at runtime), including the inlined `db.transaction` path which emits
/// `zz_db_exec_raw` directly.
pub fn needs_sqlite_link(natives: &HashSet<String>) -> bool {
    natives.iter().any(|n| is_sql_native(n))
}

/// Staticlib symbols to force-extract (`-u`) when [`needs_pg_link`].
/// One per C-visible `zz_pg_*_raw` entry point; the linker then pulls
/// their whole object (plus rustls) instead of leaving weak imports NULL.
pub const PG_LINK_SYMBOLS: &[&str] = &[
    "zz_pg_connect_raw",
    "zz_pg_exec_raw",
    "zz_pg_query_raw",
    "zz_pg_close_raw",
];

/// Staticlib symbol to force-extract (`-u`) when the float-format gate
/// fires: the C runtime references `zz_float_format_raw` weakly, and weak
/// refs alone never pull archive members.
pub const FLOAT_FMT_SYMBOL: &str = "zz_float_format_raw";

/// `extern` declarations to inject into generated C: the handle-primitive
/// header plus one declaration per used FFI symbol. Emits an empty string
/// when no FFI native is reachable so existing programs generate
/// byte-identical C.
pub fn ffi_prelude(natives: &HashSet<String>) -> String {
    let mut symbols: Vec<&str> = natives.iter().filter_map(|n| ffi_impl(n)).collect();
    symbols.sort_unstable();
    symbols.dedup();
    if symbols.is_empty() {
        return String::new();
    }
    let mut out = String::from(FFI_H);
    out.push('\n');
    for sym in symbols {
        if let Some(decl) = ffi_decl(sym) {
            out.push_str(decl);
            out.push('\n');
        }
    }
    out
}

/// C declaration for a staticlib symbol. Extended alongside [`ffi_impl`].
fn ffi_decl(symbol: &str) -> Option<&'static str> {
    match symbol {
        "zz_regexp_compile" => Some("zz_value zz_regexp_compile(zz_value pat, int *err);"),
        "zz_regexp_is_match" => {
            Some("zz_value zz_regexp_is_match(zz_value re, zz_value s, int *err);")
        }
        "zz_regexp_find" => Some("zz_value zz_regexp_find(zz_value re, zz_value s, int *err);"),
        "zz_regexp_replace_all" => {
            Some("zz_value zz_regexp_replace_all(zz_value re, zz_value s, zz_value rep, int *err);")
        }
        "zz_regexp_captures" => {
            Some("zz_value zz_regexp_captures(zz_value re, zz_value s, int *err);")
        }
        "zz_crypto_sha256" => Some("zz_value zz_crypto_sha256(zz_value s, int *err);"),
        "zz_crypto_sha256_bytes" => {
            Some("zz_value zz_crypto_sha256_bytes(zz_value b, int *err);")
        }
        "zz_encoding_base64_decode_bytes" => {
            Some("zz_value zz_encoding_base64_decode_bytes(zz_value s, int *err);")
        }
        "zz_crypto_sha512" => Some("zz_value zz_crypto_sha512(zz_value s, int *err);"),
        "zz_crypto_hmac_sha256" => {
            Some("zz_value zz_crypto_hmac_sha256(zz_value key, zz_value msg, int *err);")
        }
        "zz_crypto_random_bytes" => Some("zz_value zz_crypto_random_bytes(zz_value n, int *err);"),
        "zz_crypto_ct_eq" => Some("zz_value zz_crypto_ct_eq(zz_value a, zz_value b, int *err);"),
        "zz_crypto_argon2_hash" => Some("zz_value zz_crypto_argon2_hash(zz_value pw, int *err);"),
        "zz_crypto_argon2_verify" => {
            Some("zz_value zz_crypto_argon2_verify(zz_value hash, zz_value pw, int *err);")
        }
        "zz_crypto_bcrypt_hash" => Some("zz_value zz_crypto_bcrypt_hash(zz_value pw, int *err);"),
        "zz_crypto_bcrypt_verify" => {
            Some("zz_value zz_crypto_bcrypt_verify(zz_value hash, zz_value pw, int *err);")
        }
        "zz_crypto_ed25519_keypair" => {
            Some("zz_value zz_crypto_ed25519_keypair(zz_value unit, int *err);")
        }
        "zz_crypto_ed25519_sign" => {
            Some("zz_value zz_crypto_ed25519_sign(zz_value sk, zz_value msg, int *err);")
        }
        "zz_crypto_ed25519_verify" => Some(
            "zz_value zz_crypto_ed25519_verify(zz_value pk, zz_value msg, zz_value sig, int *err);",
        ),
        "zz_crypto_rsa_keypair" => Some("zz_value zz_crypto_rsa_keypair(zz_value unit, int *err);"),
        "zz_crypto_rsa_sign" => {
            Some("zz_value zz_crypto_rsa_sign(zz_value sk, zz_value msg, int *err);")
        }
        "zz_crypto_rsa_verify" => Some(
            "zz_value zz_crypto_rsa_verify(zz_value pk, zz_value msg, zz_value sig, int *err);",
        ),
        "zz_crypto_jwt_encode" => {
            Some("zz_value zz_crypto_jwt_encode(zz_value payload, zz_value secret, int *err);")
        }
        "zz_crypto_jwt_decode" => {
            Some("zz_value zz_crypto_jwt_decode(zz_value token, zz_value secret, int *err);")
        }
        "zz_crypto_jwt_encode_ed" => {
            Some("zz_value zz_crypto_jwt_encode_ed(zz_value payload, zz_value sk, int *err);")
        }
        "zz_crypto_jwt_decode_ed" => {
            Some("zz_value zz_crypto_jwt_decode_ed(zz_value token, zz_value pk, int *err);")
        }
        "zz_time_now_nanos" => Some("zz_value zz_time_now_nanos(zz_value unit, int *err);"),
        "zz_time_now_micros" => Some("zz_value zz_time_now_micros(zz_value unit, int *err);"),
        "zz_time_monotonic_nanos" => {
            Some("zz_value zz_time_monotonic_nanos(zz_value unit, int *err);")
        }
        "zz_time_sleep_micros" => Some("zz_value zz_time_sleep_micros(zz_value n, int *err);"),
        "zz_log_set_level" => Some("zz_value zz_log_set_level(zz_value name, int *err);"),
        "zz_log_get_level" => Some("zz_value zz_log_get_level(zz_value unit, int *err);"),
        "zz_log_set_format" => Some("zz_value zz_log_set_format(zz_value name, int *err);"),
        "zz_log_to_file" => Some("zz_value zz_log_to_file(zz_value path, int *err);"),
        "zz_log_to_stderr" => Some("zz_value zz_log_to_stderr(zz_value unit, int *err);"),
        "zz_log_trace" => Some("zz_value zz_log_trace(zz_value msg, int *err);"),
        "zz_log_debug" => Some("zz_value zz_log_debug(zz_value msg, int *err);"),
        "zz_log_info" => Some("zz_value zz_log_info(zz_value msg, int *err);"),
        "zz_log_warn" => Some("zz_value zz_log_warn(zz_value msg, int *err);"),
        "zz_log_error" => Some("zz_value zz_log_error(zz_value msg, int *err);"),
        "zz_log_span_begin" => Some("zz_value zz_log_span_begin(zz_value name, int *err);"),
        "zz_span_end" => Some("zz_value zz_span_end(zz_value id, int *err);"),
        "zz_sys_os" => Some("zz_value zz_sys_os(zz_value unit, int *err);"),
        "zz_sys_arch" => Some("zz_value zz_sys_arch(zz_value unit, int *err);"),
        "zz_sys_cpu_count" => Some("zz_value zz_sys_cpu_count(zz_value unit, int *err);"),
        "zz_sys_hostname" => Some("zz_value zz_sys_hostname(zz_value unit, int *err);"),
        "zz_sys_total_mem" => Some("zz_value zz_sys_total_mem(zz_value unit, int *err);"),
        "zz_sys_avail_mem" => Some("zz_value zz_sys_avail_mem(zz_value unit, int *err);"),
        "zz_term_enable_raw" => Some("zz_value zz_term_enable_raw(zz_value unit, int *err);"),
        "zz_term_disable_raw" => {
            Some("zz_value zz_term_disable_raw(zz_value unit, int *err);")
        }
        "zz_term_read_key" => Some("zz_value zz_term_read_key(zz_value unit, int *err);"),
        "zz_term_poll" => Some("zz_value zz_term_poll(zz_value ms, int *err);"),
        "zz_term_get_size" => Some("zz_value zz_term_get_size(zz_value unit, int *err);"),
        "zz_term_is_tty" => Some("zz_value zz_term_is_tty(zz_value unit, int *err);"),
        "zz_term_flush" => Some("zz_value zz_term_flush(zz_value unit, int *err);"),
        "zz_args_parser" => Some("zz_value zz_args_parser(zz_value unit, int *err);"),
        "zz_args_str_flag" => {
            Some("zz_value zz_args_str_flag(zz_value h, zz_value name, zz_value def, int *err);")
        }
        "zz_args_int_flag" => {
            Some("zz_value zz_args_int_flag(zz_value h, zz_value name, zz_value def, int *err);")
        }
        "zz_args_bool_flag" => {
            Some("zz_value zz_args_bool_flag(zz_value h, zz_value name, int *err);")
        }
        "zz_args_parse" => Some("zz_value zz_args_parse(zz_value h, zz_value argv, int *err);"),
        "zz_args_get_str" => Some("zz_value zz_args_get_str(zz_value h, zz_value name, int *err);"),
        "zz_args_get_int" => Some("zz_value zz_args_get_int(zz_value h, zz_value name, int *err);"),
        "zz_args_get_bool" => {
            Some("zz_value zz_args_get_bool(zz_value h, zz_value name, int *err);")
        }
        "zz_args_positional" => {
            Some("zz_value zz_args_positional(zz_value h, zz_value i, int *err);")
        }
        "zz_args_subcommand" => Some("zz_value zz_args_subcommand(zz_value h, int *err);"),
        "zz_args_help" => Some("zz_value zz_args_help(zz_value h, zz_value prog, int *err);"),
        "zz_args_error" => Some("zz_value zz_args_error(zz_value h, int *err);"),
        "zz_args_was_help" => Some("zz_value zz_args_was_help(zz_value h, int *err);"),
        "zz_process_run" => Some("zz_value zz_process_run(zz_value cmd, zz_value argv, int *err);"),
        "zz_process_run_with_env" => {
            Some("zz_value zz_process_run_with_env(zz_value cmd, zz_value argv, zz_value env, int *err);")
        }
        "zz_process_spawn" => Some("zz_value zz_process_spawn(zz_value cmd, zz_value argv, int *err);"),
        "zz_process_wait" => Some("zz_value zz_process_wait(zz_value id, int *err);"),
        "zz_process_exit" => Some("zz_value zz_process_exit(zz_value code, int *err);"),
        "zz_process_pid" => Some("zz_value zz_process_pid(zz_value unit, int *err);"),
        "zz_uuid_v4" => Some("zz_value zz_uuid_v4(zz_value unit, int *err);"),
        "zz_uuid_v7" => Some("zz_value zz_uuid_v7(zz_value unit, int *err);"),
        "zz_uuid_parse" => Some("zz_value zz_uuid_parse(zz_value s, int *err);"),
        "zz_tls_connect" => {
            Some("zz_value zz_tls_connect(zz_value addr, zz_value name, zz_value timeout, int *err);")
        }
        "zz_tls_connect_with_ca" => Some(
            "zz_value zz_tls_connect_with_ca(zz_value addr, zz_value name, zz_value ca, zz_value timeout, int *err);",
        ),
        "zz_tls_connect_insecure" => Some(
            "zz_value zz_tls_connect_insecure(zz_value addr, zz_value name, zz_value timeout, int *err);",
        ),
        "zz_tls_write" => Some("zz_value zz_tls_write(zz_value id, zz_value data, int *err);"),
        "zz_tls_write_bytes" => {
            Some("zz_value zz_tls_write_bytes(zz_value id, zz_value data, int *err);")
        }
        "zz_tls_read" => Some("zz_value zz_tls_read(zz_value id, zz_value max, int *err);"),
        "zz_tls_read_bytes" => {
            Some("zz_value zz_tls_read_bytes(zz_value id, zz_value max, int *err);")
        }
        "zz_tls_read_exact" => {
            Some("zz_value zz_tls_read_exact(zz_value id, zz_value n, int *err);")
        }
        "zz_tls_read_line" => Some("zz_value zz_tls_read_line(zz_value id, int *err);"),
        "zz_tls_set_read_timeout" => {
            Some("zz_value zz_tls_set_read_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_tls_set_write_timeout" => {
            Some("zz_value zz_tls_set_write_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_tls_close" => Some("zz_value zz_tls_close(zz_value id, int *err);"),
        "zz_tls_peer_addr" => Some("zz_value zz_tls_peer_addr(zz_value id, int *err);"),
        "zz_unix_connect" => {
            Some("zz_value zz_unix_connect(zz_value path, zz_value timeout, int *err);")
        }
        "zz_unix_listen" => Some("zz_value zz_unix_listen(zz_value path, int *err);"),
        "zz_unix_accept" => Some("zz_value zz_unix_accept(zz_value id, int *err);"),
        "zz_unix_listener_close" => {
            Some("zz_value zz_unix_listener_close(zz_value id, int *err);")
        }
        "zz_unix_write" => Some("zz_value zz_unix_write(zz_value id, zz_value data, int *err);"),
        "zz_unix_write_bytes" => {
            Some("zz_value zz_unix_write_bytes(zz_value id, zz_value data, int *err);")
        }
        "zz_unix_read" => Some("zz_value zz_unix_read(zz_value id, zz_value max, int *err);"),
        "zz_unix_read_bytes" => {
            Some("zz_value zz_unix_read_bytes(zz_value id, zz_value max, int *err);")
        }
        "zz_unix_read_exact" => {
            Some("zz_value zz_unix_read_exact(zz_value id, zz_value n, int *err);")
        }
        "zz_unix_read_line" => Some("zz_value zz_unix_read_line(zz_value id, int *err);"),
        "zz_unix_set_read_timeout" => {
            Some("zz_value zz_unix_set_read_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_unix_set_write_timeout" => {
            Some("zz_value zz_unix_set_write_timeout(zz_value id, zz_value ms, int *err);")
        }
        "zz_unix_close" => Some("zz_value zz_unix_close(zz_value id, int *err);"),
        "zz_tls_listen" => Some(
            "zz_value zz_tls_listen(zz_value addr, zz_value cert, zz_value key, int *err);",
        ),
        "zz_tls_accept" => Some("zz_value zz_tls_accept(zz_value id, int *err);"),
        "zz_tls_listener_close" => Some("zz_value zz_tls_listener_close(zz_value id, int *err);"),
        "zz_uuid_is_valid" => Some("zz_value zz_uuid_is_valid(zz_value s, int *err);"),
        _ => None,
    }
}

/// Failure to build, locate, or link the native runtime static library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiError(pub String);

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native runtime link failed: {}", self.0)
    }
}

impl std::error::Error for FfiError {}

/// Target directory honoring `CARGO_TARGET_DIR` (else `<root>/target`).
fn target_dir(root: &std::path::Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"))
}

/// Build (incrementally) and locate `libzz_native_rt.a` for `profile`
/// (`release = true` → `--release`, matching optimized AOT builds).
///
/// Fast-path: if the archive already exists and is newer than every source
/// file under `crates/zz_native_rt/src/`, skip `cargo build` entirely.
/// This turns the common case (lib already built) from 0.5-50s → ~0ms.
pub fn ensure_staticlib(release: bool) -> Result<PathBuf, FfiError> {
    resolve_staticlib(release).map(|(lib, _)| lib)
}

/// How the native runtime archive would be resolved: which strategy found
/// it and where the archive lives. Used by `zz doctor` to audit the
/// environment so contributors know whether `zz build` / `zz run --native`
/// can link without `ZZ_NATIVE_RT_DIR`.
pub fn native_rt_resolution(release: bool) -> Result<(PathBuf, RtSource, &'static str), FfiError> {
    let (lib, source) = resolve_staticlib(release)?;
    let strategy = match source {
        RtSource::Cargo => "cargo build from workspace sources",
        RtSource::Prebuilt => "shipped prebuilt archive",
    };
    Ok((lib, source, strategy))
}

/// How [`ensure_staticlib`] found the archive: workspace sources get a
/// (cached, incremental) cargo build; release installs use the shipped
/// archive directly — no cargo, no sources, no Rust toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtSource {
    Cargo,
    Prebuilt,
}

fn resolve_staticlib(release: bool) -> Result<(PathBuf, RtSource), FfiError> {
    // 1. Explicit `$ZZ_NATIVE_RT_DIR`: Cargo.toml → cargo build, bare
    // archive → use directly. Anything else is a loud misconfiguration.
    if let Some(dir) = std::env::var_os("ZZ_NATIVE_RT_DIR").map(PathBuf::from) {
        if dir.join("Cargo.toml").is_file() {
            return Ok((build_staticlib(&dir, release)?, RtSource::Cargo));
        }
        let prebuilt = dir.join(lib_file_name());
        if prebuilt.is_file() {
            return Ok((prebuilt, RtSource::Prebuilt));
        }
        return Err(FfiError(format!(
            "ZZ_NATIVE_RT_DIR={} has neither Cargo.toml nor {}; \
             point it at the workspace root or a dir with the prebuilt archive",
            dir.display(),
            lib_file_name()
        )));
    }
    // 2. Dev checkout (compile-time baked layout).
    if let Some(root) = checkout_root() {
        return Ok((build_staticlib(&root, release)?, RtSource::Cargo));
    }
    // 3. Runtime workspace-root fallback: walk up from CWD and the exe
    // directory looking for a Cargo.toml whose tree contains
    // crates/zz_native_rt. Handles release binaries whose baked
    // checkout_root() path is stale, so no ZZ_NATIVE_RT_DIR is required
    // when building from a source checkout on any machine.
    if let Some(root) = runtime_checkout_root() {
        return Ok((build_staticlib(&root, release)?, RtSource::Cargo));
    }
    // 4. Release install: archive shipped next to the binaries.
    if let Some(dir) = find_bundled_libdir() {
        return Ok((dir.join(lib_file_name()), RtSource::Prebuilt));
    }
    Err(FfiError(
        "cannot locate native runtime archive; tried:\n \
         - $ZZ_NATIVE_RT_DIR (not set or invalid)\n \
         - compile-time workspace root (stale baked path)\n \
         - runtime workspace root walk from CWD/exe dir (no Cargo.toml found)\n \
         - bundled release libdir (<exe>/lib, /usr/lib/zz)\n \
         set ZZ_NATIVE_RT_DIR to the workspace root or install a release \
         toolchain (which ships the prebuilt native runtime)"
            .to_string(),
    ))
}

/// Compile-time workspace root when it exists on disk (dev checkouts).
/// Release-installed binaries carry a stale baked path — absence is not
/// an error here, just "not a checkout".
fn checkout_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .to_path_buf();
    root.join("Cargo.toml").is_file().then_some(root)
}

/// Runtime workspace-root fallback: walk up from CWD and the exe
/// directory looking for a Cargo.toml whose tree contains
/// crates/zz_native_rt.
fn runtime_checkout_root() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    let cwd = std::env::current_dir().ok();
    let candidates: Vec<PathBuf> = exe_dir.into_iter().chain(cwd).collect();
    for start in candidates {
        let mut dir = start;
        loop {
            if dir.join("Cargo.toml").is_file()
                && dir
                    .join("crates")
                    .join("zz_native_rt")
                    .join("Cargo.toml")
                    .is_file()
            {
                return Some(dir);
            }
            match dir.parent() {
                Some(p) if p != dir => dir = p.to_path_buf(),
                _ => break,
            }
        }
    }
    None
}

/// Release-install library directories: `<exe>/lib` (manual unzip) and
/// `<exe>/../lib/zz` (AUR `/usr` layout), plus the absolute AUR path.
/// A dir qualifies by containing the archive — version skew is
/// impossible (the archive ships with the binaries that use it).
fn bundled_lib_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("lib"));
            if let Some(parent) = dir.parent() {
                out.push(parent.join("lib").join("zz"));
            }
        }
    }
    out.push(PathBuf::from("/usr/lib/zz"));
    out
}

fn find_bundled_libdir() -> Option<PathBuf> {
    bundled_lib_candidates()
        .into_iter()
        .find(|d| d.join(lib_file_name()).is_file())
}

/// Path to the native-runtime archive, without triggering a build.
/// Checks cargo target dirs (dev checkout) and bundled libdir (release install).
pub fn native_rt_archive_path() -> Option<PathBuf> {
    // Dev checkout: cargo target dirs
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let root = PathBuf::from(manifest_dir)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        for profile in ["debug", "release"] {
            let path = root.join("target").join(profile).join(lib_file_name());
            if path.is_file() {
                return Some(path);
            }
        }
    }
    // Release install: bundled libdir
    if let Some(dir) = find_bundled_libdir() {
        let path = dir.join(lib_file_name());
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Platform family for a `--target` triple (or the host when `None`).
///
/// Every linker-facing decision goes through this instead of `cfg!(...)`
/// so cross builds (`zz build --target ...`) emit the *target's* flags,
/// never the host's. `cfg!` remains the fallback only when no triple is
/// given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetOs {
    Linux,
    Macos,
    Windows,
    Other,
}

/// Classify a target triple (or the host when `None`) into a platform family.
pub fn target_os(target: Option<&str>) -> TargetOs {
    match target {
        Some(t) => {
            let t = t.to_ascii_lowercase();
            if t.contains("windows") {
                TargetOs::Windows
            } else if t.contains("darwin") || t.contains("macos") || t.contains("apple") {
                TargetOs::Macos
            } else if t.contains("linux") {
                TargetOs::Linux
            } else {
                TargetOs::Other
            }
        }
        None => {
            if cfg!(windows) {
                TargetOs::Windows
            } else if cfg!(target_os = "macos") {
                TargetOs::Macos
            } else if cfg!(target_os = "linux") {
                TargetOs::Linux
            } else {
                TargetOs::Other
            }
        }
    }
}

/// True when `name` is a Rust std static archive.
///
/// Unix and GNU Windows ship `libstd-<hash>.rlib`; the MSVC toolchain
/// ships `std-<hash>.rlib` with no `lib` prefix. Accept both so a
/// Windows release zip (or a rustc libdir on Windows) is linkable.
fn is_std_rlib_name(name: &str) -> bool {
    name.ends_with(".rlib") && (name.starts_with("libstd-") || name.starts_with("std-"))
}

/// Shipped static libstd in a release lib dir (`libstd-<hash>.rlib`,
/// `std-<hash>.rlib` on MSVC). Same newest-wins pick as the rustc-libdir
/// scan. `None` when the dir ships none (old releases shipped only the
/// shared object).
fn find_shipped_std_rlib(libdir: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(libdir).ok()?;
    let mut found: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_std_rlib_name(n))
        .collect();
    found.sort();
    found.pop()
}

/// Shipped shared libstd in a release lib dir (`libstd-<hash>.so`,
/// `.dylib` on macOS, `std-<hash>.dll`/`libstd-<hash>.dll` on Windows),
/// newest pick like the rustc-libdir scan.
/// `None` when the dir ships none (dev target dirs never do).
/// Kept as a fallback for pre-#303 releases that shipped only the
/// shared object; new releases ship the static rlib instead.
fn find_shipped_libstd(libdir: &std::path::Path, os: TargetOs) -> Option<String> {
    let suffixes: &[&str] = match os {
        TargetOs::Macos => &[".dylib"],
        TargetOs::Windows => &[".dll"],
        TargetOs::Linux | TargetOs::Other => &[".so"],
    };
    let entries = std::fs::read_dir(libdir).ok()?;
    let mut found: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            (n.starts_with("libstd-") || n.starts_with("std-"))
                && suffixes.iter().any(|s| n.ends_with(s))
        })
        .collect();
    found.sort();
    found.pop()
}

fn build_staticlib(root: &std::path::Path, release: bool) -> Result<PathBuf, FfiError> {
    let lib = target_dir(root)
        .join(if release { "release" } else { "debug" })
        .join(lib_file_name());

    // Fast-path: skip cargo build when the archive is already up-to-date.
    if lib.is_file() {
        if let Some(lib_mtime) = file_mtime(&lib) {
            let src_dir = root.join("crates").join("zz_native_rt").join("src");
            if !src_dir_mtime_newer_than(&src_dir, lib_mtime) {
                return snapshot_staticlib(&lib, release);
            }
        }
    }

    let release_flag = if release { " --release" } else { "" };
    // Progress chatter only on an interactive stderr: `zz run --native`
    // execs the program with inherited stderr, so any build log line would
    // merge with program output (breaking piped `2>` captures and the
    // dual-engine parity harness on cold caches — whichever fixture
    // triggers the one-time staticlib build gains a stray stderr line).
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        eprintln!("zz: building native runtime (cargo build -p zz_native_rt{release_flag})...");
    }
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .arg("-p")
        .arg("zz_native_rt");
    if release {
        cmd.arg("--release");
    }
    let out = cmd
        .output()
        .map_err(|e| FfiError(format!("cannot run cargo: {e}")))?;
    if !out.status.success() {
        return Err(FfiError(format!(
            "cargo build -p zz_native_rt failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    if !lib.is_file() {
        return Err(FfiError(format!(
            "static library missing after build: {}",
            lib.display()
        )));
    }
    // Release archives ship debug info (workspace profile keeps it:
    // ~200M with DWARF) and every downstream `-O3 -flto=thin` link
    // would page it all in — the link-time OOM on small boxes. The
    // final binaries are stripped anyway (`-s`), so debug info is pure
    // link cost: `snapshot_staticlib` strips the copy it publishes and
    // never mutates the cargo output (a linker may be reading it).
    snapshot_staticlib(&lib, release)
}

/// Immutable snapshot of the cargo-built archive, for linking.
///
/// The cargo output path is not stable: `cargo build` replaces the file,
/// and the debug strip used to rewrite it in place, so a concurrent linker
/// could read a torn archive — `file truncated`, `file format not
/// recognized`, or `undefined reference to zz_float_format_raw` when the
/// C runtime archive's member was already published (#548).
///
/// Every link now consumes a snapshot keyed by the archive's length + mtime:
/// copied to a private temp, stripped there, and published with one atomic
/// `rename`. A reader can only ever observe a complete, immutable file, and
/// a writer never sees a reader's path change under it. Cross-process
/// publishers of the same key race harmlessly (identical bytes, `rename`
/// picks the winner).
fn snapshot_staticlib(lib: &std::path::Path, strip: bool) -> Result<PathBuf, FfiError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    // Serialize this process's copies; the double-check under the lock
    // keeps parallel transient builds from each copying ~200M.
    static SNAPSHOT_LOCK: Mutex<()> = Mutex::new(());
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn stamp(path: &std::path::Path) -> Option<(u64, u128)> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some((meta.len(), mtime))
    }

    let Some((len, mtime)) = stamp(lib) else {
        // Missing archive: let the caller's link report it in its own
        // words instead of masking it with a snapshot error.
        return Ok(lib.to_path_buf());
    };
    let parent = lib
        .parent()
        .ok_or_else(|| FfiError(format!("static library has no parent: {}", lib.display())))?;
    let root = parent.join("zz_native_rt-snapshots");
    let key = format!("{len}-{mtime}-{}", if strip { "stripped" } else { "plain" });
    let dir = root.join(&key);
    let snap = dir.join(lib_file_name());

    // Reuse: the key pins the source archive version and the stripping
    // mode, and publishing is atomic, so any non-empty file in the key
    // dir is a complete copy of exactly this input.
    if let Some((snap_len, _)) = stamp(&snap) {
        if snap_len > 0 {
            return Ok(snap);
        }
        // Torn/garbage snapshot (killed mid-copy before the rename guard
        // existed): rebuild rather than link it.
        let _ = std::fs::remove_file(&snap);
    }

    let _guard = SNAPSHOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((snap_len, _)) = stamp(&snap) {
        if snap_len > 0 {
            return Ok(snap);
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| FfiError(format!("cannot create {}: {e}", dir.display())))?;

    // Copy only from a stable source: a writer that changes the archive
    // mid-copy (cargo building another target, another process's build)
    // must be waited out, never published. Release static libs are ~200M
    // on a cold cache, hence the generous deadline.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let before = stamp(lib);
        let tmp = dir.join(format!(
            ".{}.{}.tmp",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let copied = std::fs::copy(lib, &tmp).ok().and_then(|_| stamp(&tmp));
        let after = stamp(lib);
        let stable = match (before, copied, after) {
            (Some(b), Some(c), Some(a)) if b == a && c.0 == b.0 && c.0 > 0 => {
                // Filesystem timestamps can be coarse enough that a fast
                // truncate+write leaves `(len, mtime)` unchanged across a
                // partial write. Require the source to stay put across a
                // short settling window before trusting the copy.
                std::thread::sleep(Duration::from_millis(2));
                stamp(lib) == Some(b)
            }
            _ => false,
        };
        if stable {
            if strip {
                strip_debug_symbols(&tmp);
            }
            match std::fs::rename(&tmp, &snap) {
                Ok(()) => {
                    prune_staticlib_snapshots(&root, &key);
                    return Ok(snap);
                }
                // Lost a publish race: adopt whatever won.
                Err(_) => {
                    let _ = std::fs::remove_file(&tmp);
                    if let Some((snap_len, _)) = stamp(&snap) {
                        if snap_len > 0 {
                            return Ok(snap);
                        }
                    }
                }
            }
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        if Instant::now() >= deadline {
            return Err(FfiError(format!(
                "native runtime archive keeps changing while snapshotting: {} ({:?})",
                lib.display(),
                stamp(lib)
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Bound snapshot growth: superseded archive versions are dead weight
/// (~200M each in release). Keep the newest few — a concurrent process may
/// still be linking one it resolved moments ago — and only delete entries
/// past a grace window.
fn prune_staticlib_snapshots(root: &std::path::Path, keep: &str) {
    use std::time::{Duration, SystemTime};
    const KEEP: usize = 4;
    // A day of grace: any build that resolved an old snapshot has long
    // finished linking by then, so pruning can never pull a file out from
    // under a live `clang`.
    const GRACE: Duration = Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(SystemTime, PathBuf, String)> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            Some((mtime, e.path(), name))
        })
        .collect();
    if dirs.len() <= KEEP {
        return;
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
    let now = SystemTime::now();
    for (mtime, path, name) in dirs.into_iter().skip(KEEP) {
        if name == keep {
            continue;
        }
        if now.duration_since(mtime).map(|d| d < GRACE).unwrap_or(true) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// Drop DWARF/debug sections from `path` in place, keeping the symbol
/// table (linking unaffected). Returns whether anything was stripped.
/// Silent on all paths: failures (missing binutils, foreign formats)
/// just mean a heavier downstream link, never a build error.
fn strip_debug_symbols(path: &std::path::Path) -> bool {
    // COFF import/static libs are not handled here.
    if cfg!(windows) {
        return false;
    }
    // GNU strip uses -g, BSD/macOS strip uses -S for the same.
    let flag = if cfg!(target_vendor = "apple") {
        "-S"
    } else {
        "-g"
    };
    let before = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let status = std::process::Command::new("strip")
        .arg(flag)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => std::fs::metadata(path)
            .map(|m| m.len() < before)
            .unwrap_or(false),
        _ => false,
    }
}

/// Return the modification time of a file, or `None` on error.
fn file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// True when any `.rs` file under `dir` is newer than `threshold`.
fn src_dir_mtime_newer_than(dir: &std::path::Path, threshold: std::time::SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "rs") {
            if let Ok(meta) = std::fs::metadata(&p) {
                if let Ok(mtime) = meta.modified() {
                    if mtime > threshold {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Static library file name for the current host platform.
pub fn lib_file_name() -> &'static str {
    lib_file_name_for(target_os(None))
}

/// Static library file name for a platform family.
pub fn lib_file_name_for(os: TargetOs) -> &'static str {
    match os {
        TargetOs::Windows => "zz_native_rt.lib",
        _ => "libzz_native_rt.a",
    }
}

/// System libraries appended after the Rust archives on every link.
///
/// Linux keeps the historical set (`-lpthread -ldl -lm`). The BSDs ship
/// `dlopen` in libc — there is no `libdl`, and passing `-ldl` breaks the
/// link — so they get `-lpthread -lm` only. macOS is Unix but its C
/// stacks live in frameworks, not flat libraries: `sysinfo` (memory/disk
/// stats) needs CoreFoundation and IOKit, and `ring`'s RNG path can call
/// into Security, so those are added explicitly (an unused `-framework`
/// costs nothing). The rlib name (`std-*.rlib` / `libstd-*.rlib`) and the
/// shared-object suffix are resolved by the target platform, never the
/// host.
fn sys_link_libs(os: TargetOs) -> &'static [&'static str] {
    match os {
        TargetOs::Linux => &["-lpthread", "-ldl", "-lm"],
        TargetOs::Macos => &[
            "-lpthread",
            "-ldl",
            "-lm",
            "-framework",
            "CoreFoundation",
            "-framework",
            "IOKit",
            "-framework",
            "Security",
        ],
        // FreeBSD / OpenBSD / NetBSD / DragonFly: no libdl.
        TargetOs::Other => &["-lpthread", "-lm"],
        // Windows never reaches here (MSVC-style branch above); kept
        // total so the match is exhaustive.
        TargetOs::Windows => &[],
    }
}

/// System libraries for Windows/MSVC native FFI links.
///
/// These are .lib import libraries that the MSVC linker (or clang/zig in
/// MSVC-compatible mode on Windows) understands.
fn sys_link_libs_windows() -> &'static [&'static str] {
    &[
        "kernel32.lib",
        "user32.lib",
        "advapi32.lib",
        "ws2_32.lib",
        "crypt32.lib",
    ]
}

/// `rustc`'s platform library directory (home of `libstd-*.rlib`).
/// With `target`, the target's libdir (`--target <triple>`); otherwise
/// the host's. Cross builds needing the native runtime must have the
/// target's std installed (`rustup target add <triple>`).
fn rustc_libdir_for_target(target: Option<&str>) -> Result<PathBuf, FfiError> {
    let mut cmd = Command::new("rustc");
    cmd.arg("--print").arg("target-libdir");
    if let Some(t) = target {
        cmd.arg("--target").arg(t);
    }
    let out = cmd
        .output()
        .map_err(|e| FfiError(format!("cannot run rustc: {e}")))?;
    if !out.status.success() {
        return Err(FfiError("rustc --print target-libdir failed".into()));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Exact `libstd` static archive file name in `libdir`
/// (`libstd-<hash>.rlib` on Unix/GNU, `std-<hash>.rlib` on MSVC). The
/// rlib links directly through `cc` like a normal archive (verified:
/// `clang main.c libstd-<hash>.rlib` yields no `libstd` NEEDED entry), so
/// AOT binaries carry no libstd dependency and no RUNPATH.
fn find_std_rlib(libdir: &std::path::Path) -> Result<String, FfiError> {
    let entries =
        std::fs::read_dir(libdir).map_err(|e| FfiError(format!("cannot read {libdir:?}: {e}")))?;
    let mut found: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_std_rlib_name(n))
        .collect();
    found.sort();
    found.pop().ok_or_else(|| {
        FfiError(format!(
            "no std-*.rlib in {} (rustc libdir); native FFI link needs the static libstd",
            libdir.display()
        ))
    })
}

/// Exact `libstd` shared-object file name in `libdir` (`-l:` needs the full
/// name because rustc hashes it: `libstd-<hash>.so`).
/// Fallback for pre-#303 toolchains that shipped only the shared object.
fn find_libstd(libdir: &std::path::Path, os: TargetOs) -> Result<String, FfiError> {
    let suffix = match os {
        TargetOs::Macos => ".dylib",
        TargetOs::Windows => ".dll",
        _ => ".so",
    };
    let entries =
        std::fs::read_dir(libdir).map_err(|e| FfiError(format!("cannot read {libdir:?}: {e}")))?;
    let mut found: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| (n.starts_with("libstd-") || n.starts_with("std-")) && n.ends_with(suffix))
        .collect();
    found.sort();
    found.pop().ok_or_else(|| {
        FfiError(format!(
            "no std-*{suffix} in {} (rustc libdir); native FFI link needs the shared libstd",
            libdir.display()
        ))
    })
}

/// Extra `cc` flags to link the native runtime: the static library, the
/// static libstd archive (no NEEDED entry, no RUNPATH — #303), and
/// thread/dl helpers.
///
/// Returned flags are appended after the program object on the `cc` command
/// line. Windows targets get the archive by absolute path plus MSVC import
/// libraries (the driver is GNU-flavoured `clang`/`zig cc`, not clang-cl).
pub fn link_args(release: bool) -> Result<(Vec<String>, PathBuf), FfiError> {
    link_args_for_target(release, None)
}

/// [`link_args`] scoped to a `--target=<triple>` cross triple: the
/// target's std rlib is used so cross builds never mix host objects in.
pub fn link_args_for_target(
    release: bool,
    target: Option<&str>,
) -> Result<(Vec<String>, PathBuf), FfiError> {
    let (lib, _source) = resolve_staticlib(release)?;
    let libdir = lib
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| FfiError(format!("static library has no parent: {}", lib.display())))?;

    // Target-aware platform detection: use the --target triple when
    // provided, otherwise fall back to the host. This matters for
    // cross-compilation (e.g. building Windows targets from Linux).
    let os = target_os(target);

    // Windows/MSVC linking. The compiler driver is the GNU-flavoured
    // `clang` (or `zig cc`), not clang-cl: only `-L`/`-l` are translated to
    // `/LIBPATH:`/`foo.lib`. Passing `/LIBPATH:` or a bare relative
    // `foo.lib` makes the driver treat them as input files
    // (`error: no such file`), so the archive goes in by absolute path.
    if os == TargetOs::Windows {
        let mut base = vec![libdir.join(lib_file_name_for(os)).display().to_string()];
        base.extend(sys_link_libs_windows().iter().map(|s| s.to_string()));
        // Release installs ship the static libstd next to the archive.
        if let Some(shipped) = find_shipped_std_rlib(&libdir) {
            let mut flags = base.clone();
            flags.insert(1, libdir.join(&shipped).display().to_string());
            return Ok((flags, lib));
        }
        if let Ok(rustc_dir) = rustc_libdir_for_target(target) {
            if let Ok(rlib) = find_std_rlib(&rustc_dir) {
                let mut flags = base.clone();
                flags.insert(1, rustc_dir.join(&rlib).display().to_string());
                return Ok((flags, lib));
            }
        }
        return Err(FfiError(format!(
            "no std-*.rlib in {} or rustc libdir; native FFI link on Windows needs the static libstd",
            libdir.display()
        )));
    }

    // Unix/macOS linking: -L and -l flags.
    // Base flags shared by every branch: the archive search path, the
    // native runtime itself, then the platform system libs
    // (`-ldl` omitted on the BSDs — `dlopen` lives in libc there).
    let mut base = vec![
        format!("-L{}", libdir.display()),
        "-lzz_native_rt".to_string(),
    ];
    base.extend(sys_link_libs(os).iter().map(|s| s.to_string()));
    // Release installs ship the static libstd next to the archive: link
    // it by absolute path — no rustc, no RUNPATH, no sidecar. The `ldd`
    // output shows no `libstd` line (glibc/libm dynamic is fine).
    if let Some(shipped) = find_shipped_std_rlib(&libdir) {
        let mut flags = base.clone();
        flags.insert(2, libdir.join(&shipped).display().to_string());
        return Ok((flags, lib));
    }
    if let Ok(rustc_dir) = rustc_libdir_for_target(target) {
        if let Ok(rlib) = find_std_rlib(&rustc_dir) {
            let mut flags = base.clone();
            flags.insert(2, rustc_dir.join(&rlib).display().to_string());
            return Ok((flags, lib));
        }
    }
    // Fallback: pre-#303 layout with only the shared object (old release
    // zips, or a toolchain without rlibs). Native-only like the static
    // shipped path above — cross builds error loudly on the target
    // sysroot instead of mixing host objects.
    if let Some(shipped) = find_shipped_libstd(&libdir, os) {
        let mut flags = base.clone();
        flags.insert(2, format!("-L{}", libdir.display()));
        flags.insert(3, format!("-l:{shipped}"));
        return Ok((flags, lib));
    }
    let rustc_dir = rustc_libdir_for_target(target).map_err(|e| {
        FfiError(format!(
            "{e}{}",
            target
                .map(|t| format!(" (`rustup target add {t}` for --target {t})"))
                .unwrap_or_default()
        ))
    })?;
    // Static rlib preferred; shared object only as a last resort.
    if let Ok(rlib) = find_std_rlib(&rustc_dir) {
        let mut flags = base.clone();
        flags.insert(2, rustc_dir.join(&rlib).display().to_string());
        return Ok((flags, lib));
    }
    if let Ok(libstd) = find_libstd(&rustc_dir, os) {
        let mut flags = base;
        flags.insert(2, format!("-L{}", rustc_dir.display()));
        flags.insert(3, format!("-l:{libstd}"));
        flags.insert(4, format!("-Wl,-rpath,{}", rustc_dir.display()));
        return Ok((flags, lib));
    }
    Err(FfiError(format!(
        "no libstd-*.rlib in {} (rustc libdir); native FFI link needs the static libstd{}",
        rustc_dir.display(),
        target
            .map(|t| format!(" for --target {t} (`rustup target add {t}`)"))
            .unwrap_or_default()
    )))
}

/// Copy shipped shared libs next to the output binary and return the
/// loader path flag. Static links (#303 default) stage nothing — there
/// is no shared object to sidecar. Only the legacy shared-libstd
/// fallback produces a sidecar; dev checkouts keep the rustc rpath from
/// [`link_args`] in that fallback.
///
/// Target-aware: a cross build stages the *target's* sidecar and emits
/// the target's loader flag (`@loader_path` on macOS, `$ORIGIN` on
/// Linux/BSD, nothing on Windows — the exe directory is searched first).
pub fn stage_shared_libs(
    lib_path: &std::path::Path,
    out_bin: &std::path::Path,
    target: Option<&str>,
) -> Result<Vec<String>, FfiError> {
    let os = target_os(target);
    let Some(libdir) = lib_path.parent() else {
        return Ok(vec![]);
    };
    let Some(shipped) = find_shipped_libstd(libdir, os) else {
        return Ok(vec![]);
    };
    let Some(out_dir) = out_bin.parent() else {
        return Ok(vec![]);
    };
    let dest = out_dir.join(&shipped);
    let copy = match (
        std::fs::metadata(libdir.join(&shipped)),
        std::fs::metadata(&dest),
    ) {
        (Ok(src), Ok(dst)) => src.len() != dst.len(),
        _ => true,
    };
    if copy {
        std::fs::copy(libdir.join(&shipped), &dest)
            .map_err(|e| FfiError(format!("cannot stage {}: {e}", dest.display())))?;
    }
    match os {
        // Windows searches the exe directory for DLLs — no rpath needed.
        TargetOs::Windows => Ok(vec![]),
        // `$ORIGIN` (Linux/BSD) / `@loader_path` (macOS): the binary finds
        // its sidecar wherever the user puts the pair. No LD_LIBRARY_PATH,
        // no absolute toolchain paths.
        TargetOs::Macos => Ok(vec!["-Wl,-rpath,@loader_path".to_string()]),
        TargetOs::Linux | TargetOs::Other => Ok(vec!["-Wl,-rpath,$ORIGIN".to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelude_empty_without_ffi_natives() {
        let natives: HashSet<String> = ["println".to_string(), "std.time.now_ms".to_string()]
            .into_iter()
            .collect();
        assert!(!needs_native_rt(&natives));
        assert_eq!(ffi_prelude(&natives), "");
        // Unknown future names without a registry entry stay embedded-only.
        assert_eq!(ffi_impl("std.quic.connect"), None);
    }

    #[test]
    fn prelude_declares_used_ffi_symbols() {
        let natives: HashSet<String> = [
            "std.regexp.compile".to_string(),
            "regexp.is_match".to_string(),
        ]
        .into_iter()
        .collect();
        assert!(needs_native_rt(&natives));
        let pre = ffi_prelude(&natives);
        assert!(pre.contains("zz_rt_handle_alloc"));
        assert!(pre.contains("zz_value zz_regexp_compile(zz_value pat, int *err);"));
        assert!(pre.contains("zz_value zz_regexp_is_match(zz_value re, zz_value s, int *err);"));
        assert!(!pre.contains("zz_regexp_find"));
    }

    #[test]
    fn bundled_libdir_detection() {
        let base = std::env::temp_dir().join(format!(
            "zz-ffi-bundled-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let libdir = base.join("lib");
        std::fs::create_dir_all(&libdir).expect("mkdir");
        // Empty dir: no shipped libstd of either kind.
        assert_eq!(find_shipped_std_rlib(&libdir), None);
        let host_os = target_os(None);
        assert_eq!(find_shipped_libstd(&libdir, host_os), None);
        // Archive alone: still no shared objects.
        std::fs::write(libdir.join(lib_file_name()), b"fake-archive").expect("write");
        assert_eq!(find_shipped_std_rlib(&libdir), None);
        assert_eq!(find_shipped_libstd(&libdir, host_os), None);
        // Shipped static rlib detected, newest wins (#303 default).
        std::fs::write(libdir.join("libstd-aaa.rlib"), b"old").expect("write");
        std::fs::write(libdir.join("libstd-zzz.rlib"), b"new").expect("write");
        assert_eq!(
            find_shipped_std_rlib(&libdir),
            Some("libstd-zzz.rlib".to_string())
        );
        // MSVC ships `std-<hash>.rlib` (no `lib` prefix) — must be found too.
        std::fs::write(libdir.join("std-mmm.rlib"), b"msvc").expect("write");
        assert_eq!(
            find_shipped_std_rlib(&libdir),
            Some("std-mmm.rlib".to_string())
        );
        std::fs::remove_file(libdir.join("std-mmm.rlib")).ok();
        // Legacy shared object still detected as a fallback.
        let (prefix, suffix) = match host_os {
            TargetOs::Macos => ("libstd-", ".dylib"),
            TargetOs::Windows => ("std-", ".dll"),
            _ => ("libstd-", ".so"),
        };
        std::fs::write(libdir.join(format!("{prefix}aaa{suffix}")), b"old").expect("write");
        std::fs::write(libdir.join(format!("{prefix}zzz{suffix}")), b"new").expect("write");
        assert_eq!(
            find_shipped_libstd(&libdir, host_os),
            Some(format!("{prefix}zzz{suffix}"))
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn std_rlib_names_cover_msvc_and_gnu() {
        assert!(is_std_rlib_name("libstd-1a2b.rlib"));
        assert!(is_std_rlib_name("std-1a2b.rlib"));
        assert!(!is_std_rlib_name("libstd-1a2b.rlib.bak"));
        assert!(!is_std_rlib_name("std-1a2b.dll"));
        assert!(!is_std_rlib_name("other.rlib"));
    }

    #[test]
    fn target_os_classifies_triples() {
        assert_eq!(target_os(Some("x86_64-pc-windows-msvc")), TargetOs::Windows);
        assert_eq!(
            target_os(Some("aarch64-pc-windows-msvc")),
            TargetOs::Windows
        );
        assert_eq!(target_os(Some("aarch64-apple-darwin")), TargetOs::Macos);
        assert_eq!(target_os(Some("x86_64-unknown-linux-gnu")), TargetOs::Linux);
        assert_eq!(target_os(Some("x86_64-unknown-freebsd")), TargetOs::Other);
    }

    #[test]
    fn static_link_prefers_rlib_without_rpath() {
        // The #303 contract: static libstd, no `-l:`, no RUNPATH.
        let (args, _lib) = link_args(false).expect("link_args");
        let joined = args.join(" ");
        assert!(
            joined.contains(".rlib"),
            "static link must pass the rlib path: {joined}"
        );
        assert!(
            !joined.contains("-l:"),
            "static link must not use -l:libstd.so: {joined}"
        );
        assert!(
            !args.iter().any(|a| a.contains("-rpath")),
            "static link must emit no rpath: {joined}"
        );
    }

    #[test]
    fn stage_shared_libs_sidecars() {
        let base = std::env::temp_dir().join(format!(
            "zz-ffi-stage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let libdir = base.join("lib");
        let outdir = base.join("out");
        std::fs::create_dir_all(&libdir).expect("mkdir");
        std::fs::create_dir_all(&outdir).expect("mkdir");
        std::fs::write(libdir.join(lib_file_name()), b"fake-archive").expect("write");
        let host_os = target_os(None);
        let (prefix, suffix) = match host_os {
            TargetOs::Macos => ("libstd-", ".dylib"),
            TargetOs::Windows => ("std-", ".dll"),
            _ => ("libstd-", ".so"),
        };
        let so = format!("{prefix}zzz{suffix}");
        std::fs::write(libdir.join(&so), b"fake-so").expect("write");
        let out_bin = outdir.join("prog");
        let flags =
            stage_shared_libs(&libdir.join(lib_file_name()), &out_bin, None).expect("stage");
        assert!(outdir.join(&so).is_file(), "sidecar copied");
        match host_os {
            TargetOs::Macos => {
                assert_eq!(flags, vec!["-Wl,-rpath,@loader_path".to_string()])
            }
            TargetOs::Windows => assert!(flags.is_empty(), "no rpath on Windows: {flags:?}"),
            _ => assert_eq!(flags, vec!["-Wl,-rpath,$ORIGIN".to_string()]),
        }
        // No shipped objects: empty flags, nothing copied.
        std::fs::remove_file(libdir.join(&so)).ok();
        std::fs::remove_file(outdir.join(&so)).ok();
        let flags =
            stage_shared_libs(&libdir.join(lib_file_name()), &out_bin, None).expect("stage");
        assert!(flags.is_empty());
        assert!(!outdir.join(&so).is_file());
        // Cross-target: a Windows target stages a `.dll` and emits no rpath,
        // regardless of the host.
        std::fs::write(libdir.join("std-cross.dll"), b"fake-dll").expect("write");
        let flags = stage_shared_libs(
            &libdir.join(lib_file_name()),
            &out_bin,
            Some("x86_64-pc-windows-msvc"),
        )
        .expect("stage");
        assert!(flags.is_empty(), "Windows cross build needs no rpath");
        assert!(outdir.join("std-cross.dll").is_file());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn system_lib_needs_follow_reachability() {
        let set =
            |names: &[&str]| -> HashSet<String> { names.iter().map(|s| s.to_string()).collect() };
        // Plain programs need neither system lib.
        let plain = set(&["println", "std.time.now_ms"]);
        assert!(!needs_curl_link(&plain));
        assert!(!needs_sqlite_link(&plain));
        // Outbound client needs curl only (bare + std spellings).
        for n in ["http.fetch", "std.http.fetch", "http.get", "std.http.post"] {
            let s = set(&[n]);
            assert!(needs_curl_link(&s), "{n} must need curl");
            assert!(!needs_sqlite_link(&s), "{n} must not need sqlite");
        }
        // Queries need sqlite only (all handle spellings + transaction).
        for n in [
            "sqlz.open",
            "std.sqlz.open",
            "pg.query",
            "std.sqlz.postgres.connect",
        ] {
            let s = set(&[n]);
            assert!(needs_sqlite_link(&s), "{n} must need sqlite");
            assert!(!needs_curl_link(&s), "{n} must not need curl");
        }
        // Server-only HTTP surface needs neither (plain epoll + libc).
        let server = set(&["http.server", "std.http.listen", "http.route_get"]);
        assert!(!needs_curl_link(&server));
        assert!(!needs_sqlite_link(&server));
    }

    #[test]
    fn header_declares_handle_primitives() {
        for sym in [
            "zz_rt_version",
            "zz_rt_handle_alloc",
            "zz_rt_handle_drop",
            "zz_rt_handle_live",
            "zz_rt_handle_tag_eq",
        ] {
            assert!(FFI_H.contains(sym), "FFI_H missing {sym}");
        }
        assert_eq!(FFI_VERSION, zz_native_rt::FFI_VERSION);
    }

    /// End-to-end proof of the AOT link mechanism: build the real static
    /// library, compile a C program that combines the real runtime TU
    /// (`RUNTIME_C`, exactly as generated programs do) with FFI calls, link
    /// with the real flags, run it, and check handle alloc/tag/drop plus a
    /// crypto call (which exercises the linkable `zz_str_*` constructors)
    /// across the language boundary.
    #[test]
    fn link_staticlib_from_c() {
        let (mut args, _rt_lib) = match link_args(false) {
            Ok(a) => a,
            Err(e) => panic!("link_args failed: {e}"),
        };
        // Same runtime deps as compile::build (vcpkg names curl's import
        // library `libcurl.lib` on Windows).
        args.push(if cfg!(windows) {
            "-llibcurl".to_string()
        } else {
            "-lcurl".to_string()
        });
        args.push("-lsqlite3".to_string());
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let tmp = std::env::temp_dir().join(format!("zz-ffi-test-{}-{uniq}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("tmpdir");
        let c_path = tmp.join("t.c");
        let bin_path = tmp.join("t");
        let used: HashSet<String> = ["std.crypto.sha256".to_string()].into_iter().collect();
        let prelude = ffi_prelude(&used);
        assert!(
            prelude.contains("zz_crypto_sha256"),
            "prelude must declare used symbols"
        );
        let raw = format!(
            "{headers}\n{runtime}\n{prelude}\n#include <stdlib.h>\nvoid zz_main(void) {{\n    if (zz_rt_version() != {ver}ULL) exit(10);\n    uint64_t live0 = zz_rt_handle_live();\n    uint64_t id = zz_rt_handle_alloc((const uint8_t *)\"regex\", 5);\n    if (id == 0) exit(11);\n    if (!zz_rt_handle_tag_eq(id, (const uint8_t *)\"regex\", 5)) exit(12);\n    if (zz_rt_handle_tag_eq(id, (const uint8_t *)\"uuid\", 4)) exit(13);\n    if (zz_rt_handle_live() != live0 + 1) exit(14);\n    if (!zz_rt_handle_drop(id)) exit(15);\n    if (zz_rt_handle_tag_eq(id, (const uint8_t *)\"regex\", 5)) exit(16);\n    int err = 0;\n    zz_value digest = zz_crypto_sha256(zz_str_new(\"abc\", 3), &err);\n    const char *ptr = NULL; size_t len = 0;\n    zz_str_view(digest, &ptr, &len);\n    if (err != 0 || len != 64 || ptr == NULL) exit(17);\n    if (memcmp(ptr, \"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\", 64) != 0) exit(18);\n    printf(\"ffi_link_ok\\n\");\n}}\nint zz_call_main(void) {{ return 0; }}\n",
            headers = crate::RUNTIME_H,
            runtime = crate::RUNTIME_C,
            ver = FFI_VERSION
        );
        // Same single-TU assembly as generated programs: drop quoted
        // includes (headers are concatenated, not on disk).
        let src = crate::lower::strip_quoted_includes(&raw);
        std::fs::write(&c_path, &src).expect("write C");
        let cc = crate::detect_clang().expect("no C compiler");
        let mut cmd = Command::new(&cc.path);
        if cc.zig {
            cmd.arg("cc");
        }
        cmd.arg("-O1")
            .arg("-DZZ_HAS_CURL=0")
            .arg("-DZZ_HAS_SQLITE3=0")
            .arg("-o")
            .arg(&bin_path)
            .arg(&c_path);
        for a in &args {
            cmd.arg(a);
        }
        let out = cmd.output().expect("run cc");
        assert!(
            out.status.success(),
            "cc link failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(&bin_path).output().expect("run binary");
        assert_eq!(run.status.code(), Some(0), "exit != 0");
        assert_eq!(String::from_utf8_lossy(&run.stdout), "ffi_link_ok\n");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `strip_debug_symbols` shrinks a debug-laden archive and fails
    /// gracefully on non-archives. This is the link-time OOM guard for
    /// small boxes: the release `libzz_native_rt.a` carries ~200M with
    /// DWARF (workspace profile keeps debuginfo) and every downstream
    /// `-O3 -flto=thin` link pages it in.
    #[test]
    #[cfg(not(windows))]
    fn strip_debug_symbols_shrinks_debug_archive() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::temp_dir().join(format!(
            "zz-strip-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let cleanup = || {
            std::fs::remove_dir_all(&tmp).ok();
        };
        let src = tmp.join("tiny.c");
        std::fs::write(&src, "int zz_strip_probe_fn(void){return 41;}\n").unwrap();
        // No C toolchain here (minimal images): skip, don't fail —
        // the helper itself degrades gracefully by contract.
        let cc = std::process::Command::new("cc")
            .arg("-g")
            .arg("-c")
            .arg(&src)
            .arg("-o")
            .arg(tmp.join("tiny.o"))
            .output();
        let Ok(cc) = cc else {
            cleanup();
            return;
        };
        if !cc.status.success() {
            cleanup();
            return;
        }
        let ar = std::process::Command::new("ar")
            .arg("rcs")
            .arg(tmp.join("tiny.a"))
            .arg(tmp.join("tiny.o"))
            .output();
        let Ok(ar) = ar else {
            cleanup();
            return;
        };
        if !ar.status.success() {
            cleanup();
            return;
        }
        let arch = tmp.join("tiny.a");
        let before = std::fs::metadata(&arch).unwrap().len();
        assert!(
            strip_debug_symbols(&arch),
            "expected the debug archive to shrink"
        );
        let after = std::fs::metadata(&arch).unwrap().len();
        assert!(
            after < before,
            "stripped archive ({after}) not smaller than debug one ({before})"
        );
        // Non-archives fail gracefully (false, no panic, no rewrite):
        // link inputs stay usable, downstream links just stay heavy.
        assert!(!strip_debug_symbols(&src));
        // Snapshot publishing (#548): the linker gets a stripped private
        // copy under a content-keyed name, the cargo archive is never
        // mutated, and a second call reuses the published snapshot.
        let tiny2 = tmp.join("tiny2.a");
        let ar2 = std::process::Command::new("ar")
            .arg("rcs")
            .arg(&tiny2)
            .arg(tmp.join("tiny.o"))
            .output()
            .expect("run ar");
        assert!(ar2.status.success());
        let src_len = std::fs::metadata(&tiny2).unwrap().len();
        let snapshot = snapshot_staticlib(&tiny2, true).expect("snapshot");
        assert_ne!(snapshot, tiny2);
        assert_eq!(
            std::fs::metadata(&tiny2).unwrap().len(),
            src_len,
            "snapshot must not mutate the cargo archive"
        );
        let snap_len = std::fs::metadata(&snapshot).unwrap().len();
        assert!(
            snap_len < src_len,
            "snapshot not stripped: {snap_len} >= {src_len}"
        );
        let snap_mtime = std::fs::metadata(&snapshot).unwrap().modified().unwrap();
        assert_eq!(
            snapshot_staticlib(&tiny2, true).expect("snapshot"),
            snapshot
        );
        assert_eq!(
            std::fs::metadata(&snapshot).unwrap().modified().unwrap(),
            snap_mtime,
            "reuse must not rewrite the snapshot"
        );
        cleanup();
    }

    /// #548, reader half: parallel snapshotting while the archive is being
    /// replaced must never yield a torn copy or the live path. Guards the
    /// retry/stability logic — a snapshot is always exactly one complete
    /// writer payload.
    #[test]
    fn snapshot_staticlib_publishes_immutable_copies() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::time::Duration;
        static N: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::temp_dir().join(format!(
            "zz-snapshot-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let lib = tmp.join(lib_file_name());
        // Two distinct payloads (different lengths on purpose), both
        // written by truncate + write — how cargo replaces the archive.
        let a = vec![b'a'; 48 * 1024];
        let b: Vec<u8> = (0..32 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(&lib, &a).unwrap();

        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                for round in 0..2000usize {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::fs::write(&lib, if round % 2 == 0 { &a } else { &b }).unwrap();
                    // Leave windows where the archive is stable, so this
                    // exercises both "copy now" and "wait out the writer".
                    std::thread::sleep(Duration::from_micros(200));
                }
            });
            let readers: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        for _ in 0..25 {
                            let snap = snapshot_staticlib(&lib, false).expect("snapshot");
                            assert_ne!(snap, lib, "snapshot must never be the live archive");
                            let bytes = std::fs::read(&snap).expect("read snapshot");
                            assert!(
                                bytes == a || bytes == b,
                                "torn snapshot: {} bytes",
                                bytes.len()
                            );
                        }
                    })
                })
                .collect();
            for h in readers {
                h.join().unwrap();
            }
            stop.store(true, Ordering::Relaxed);
        });
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn native_rt_resolution_returns_archive_path() {
        // native_rt_resolution should either resolve an archive (cargo
        // checkout or bundled libdir) or fail cleanly with all strategies
        // named. On this dev checkout it resolves via the cargo target dir.
        match native_rt_resolution(false) {
            Ok((lib, source, strategy)) => {
                assert!(
                    lib.is_file(),
                    "resolved archive must exist: {}",
                    lib.display()
                );
                assert!(matches!(source, RtSource::Cargo | RtSource::Prebuilt));
                assert!(!strategy.is_empty(), "strategy must be described");
            }
            Err(e) => {
                // On a checkout without a built archive, this is acceptable
                // — but the error must name every strategy tried.
                let msg = e.0;
                assert!(
                    msg.contains("ZZ_NATIVE_RT_DIR") || msg.contains("workspace root"),
                    "error must name strategies, got: {msg}"
                );
            }
        }
    }

    #[test]
    fn runtime_checkout_root_finds_workspace() {
        // From within the zz checkout, runtime_checkout_root must find the
        // workspace root (the dir containing Cargo.toml with
        // crates/zz_native_rt/Cargo.toml).
        let root = runtime_checkout_root();
        assert!(root.is_some(), "must find workspace root from CWD");
        let root = root.unwrap();
        assert!(
            root.join("Cargo.toml").is_file(),
            "root must have Cargo.toml"
        );
        assert!(
            root.join("crates")
                .join("zz_native_rt")
                .join("Cargo.toml")
                .is_file(),
            "root must contain crates/zz_native_rt"
        );
    }

    #[test]
    fn lib_file_name_platform_correct() {
        let name = lib_file_name();
        if cfg!(windows) {
            assert_eq!(name, "zz_native_rt.lib", "Windows uses .lib");
        } else {
            assert_eq!(name, "libzz_native_rt.a", "Unix/macOS uses .a");
        }
        assert!(!name.is_empty());
    }
}
