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
//! own cache makes repeat invocations cheap) and locates the archive;
//! [`link_args`] returns the extra `cc` flags. Both are shared by `zz build`
//! and by the link test below, so the test exercises the real link path.

use std::collections::HashSet;
use std::path::PathBuf;
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
        "regexp.is_match" | "std.regexp.is_match" => Some("zz_regexp_is_match"),
        "regexp.find" | "std.regexp.find" => Some("zz_regexp_find"),
        "regexp.replace_all" | "std.regexp.replace_all" => Some("zz_regexp_replace_all"),
        "regexp.captures" | "std.regexp.captures" => Some("zz_regexp_captures"),
        "crypto.sha256" | "std.crypto.sha256" => Some("zz_crypto_sha256"),
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
pub fn needs_native_rt(natives: &HashSet<String>) -> bool {
    natives
        .iter()
        .filter_map(|n| ffi_impl(n))
        .any(|s| s != "zz_env_args")
}

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

/// Workspace root (directory containing the top-level `Cargo.toml`).
///
/// Resolution order: `$ZZ_NATIVE_RT_DIR` (used as the target-profile
/// directory's parent explicitly), otherwise the compile-time workspace
/// layout (`crates/zz_codegen` → two levels up). Installed binaries running
/// outside a checkout must set `ZZ_NATIVE_RT_DIR`.
fn workspace_root() -> Result<PathBuf, FfiError> {
    if let Some(dir) = std::env::var_os("ZZ_NATIVE_RT_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let baked = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    match baked {
        Some(root) if root.join("Cargo.toml").is_file() => Ok(root),
        _ => Err(FfiError(
            "cannot locate workspace (no Cargo.toml above zz_codegen); \
             set ZZ_NATIVE_RT_DIR to the workspace root"
                .to_string(),
        )),
    }
}

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
    let root = workspace_root()?;
    let profile = if release { "release" } else { "debug" };
    let lib = target_dir(&root).join(profile).join(lib_file_name());

    // Fast-path: skip cargo build when the archive is already up-to-date.
    if lib.is_file() {
        if let Some(lib_mtime) = file_mtime(&lib) {
            let src_dir = root.join("crates").join("zz_native_rt").join("src");
            if !src_dir_mtime_newer_than(&src_dir, lib_mtime) {
                return Ok(lib);
            }
        }
    }

    let release_flag = if release { " --release" } else { "" };
    eprintln!("zz: building native runtime (cargo build -p zz_native_rt{release_flag})...");
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
    Ok(lib)
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

/// Static library file name for the current platform.
fn lib_file_name() -> &'static str {
    if cfg!(windows) {
        "zz_native_rt.lib"
    } else {
        "libzz_native_rt.a"
    }
}

/// `rustc`'s platform library directory (home of `libstd-*.so`).
fn rustc_libdir() -> Result<PathBuf, FfiError> {
    let out = Command::new("rustc")
        .arg("--print")
        .arg("target-libdir")
        .output()
        .map_err(|e| FfiError(format!("cannot run rustc: {e}")))?;
    if !out.status.success() {
        return Err(FfiError("rustc --print target-libdir failed".into()));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Exact `libstd` shared-object file name in `libdir` (`-l:` needs the full
/// name because rustc hashes it: `libstd-<hash>.so`).
fn find_libstd(libdir: &std::path::Path) -> Result<String, FfiError> {
    let (prefix, suffix) = if cfg!(target_os = "macos") {
        ("libstd-", ".dylib")
    } else {
        ("libstd-", ".so")
    };
    let entries =
        std::fs::read_dir(libdir).map_err(|e| FfiError(format!("cannot read {libdir:?}: {e}")))?;
    let mut found: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
        .collect();
    found.sort();
    found.pop().ok_or_else(|| {
        FfiError(format!(
            "no {prefix}*{suffix} in {} (rustc libdir); native FFI link needs the shared libstd",
            libdir.display()
        ))
    })
}

/// Extra `cc` flags to link the native runtime: the static library, the
/// shared libstd (with rpath so AOT binaries run without `LD_LIBRARY_PATH`),
/// and thread/dl helpers.
///
/// Returned flags are appended after the program object on the `cc` command
/// line. Fails on Windows (MSVC import-library story is unimplemented).
pub fn link_args(release: bool) -> Result<Vec<String>, FfiError> {
    if cfg!(windows) {
        return Err(FfiError(
            "native FFI link is not implemented on Windows yet".into(),
        ));
    }
    let lib = ensure_staticlib(release)?;
    let libdir = lib
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| FfiError(format!("static library has no parent: {}", lib.display())))?;
    let rustc_dir = rustc_libdir()?;
    let libstd = find_libstd(&rustc_dir)?;
    Ok(vec![
        format!("-L{}", libdir.display()),
        "-lzz_native_rt".to_string(),
        format!("-L{}", rustc_dir.display()),
        format!("-l:{libstd}"),
        format!("-Wl,-rpath,{}", rustc_dir.display()),
        "-lpthread".to_string(),
        "-ldl".to_string(),
        "-lm".to_string(),
    ])
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
        let mut args = match link_args(false) {
            Ok(a) => a,
            Err(e) => panic!("link_args failed: {e}"),
        };
        // Same runtime deps as compile::build.
        args.push("-lcurl".to_string());
        args.push("-lsqlite3".to_string());
        let tmp = std::env::temp_dir().join(format!("zz-ffi-test-{}", std::process::id()));
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
            .arg("-DZZ_HAS_SQLITE3")
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
}
