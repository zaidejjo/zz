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
        _ => None,
    }
}

/// True when any reachable native is provided by the Rust static library
/// rather than the embedded C runtime. The build links `libzz_native_rt.a`
/// exactly in that case (plus an explicit opt-in via
/// [`crate::BuildOptions::native_rt`]).
pub fn needs_native_rt(natives: &HashSet<String>) -> bool {
    natives.iter().any(|n| ffi_impl(n).is_some())
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
pub fn ensure_staticlib(release: bool) -> Result<PathBuf, FfiError> {
    let root = workspace_root()?;
    let profile = if release { "release" } else { "debug" };
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
    let lib = target_dir(&root).join(profile).join(lib_file_name());
    if !lib.is_file() {
        return Err(FfiError(format!(
            "static library missing after build: {}",
            lib.display()
        )));
    }
    Ok(lib)
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
        let natives: HashSet<String> =
            ["std.io.println".to_string(), "std.time.now_ms".to_string()]
                .into_iter()
                .collect();
        assert!(!needs_native_rt(&natives));
        assert_eq!(ffi_prelude(&natives), "");
        // Unknown future names without a registry entry stay embedded-only.
        assert_eq!(ffi_impl("std.uuid.v4"), None);
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
    /// library, compile a C program against the real header with the real
    /// link flags, run it, and check handle alloc/tag/drop across the
    /// language boundary.
    #[test]
    fn link_staticlib_from_c() {
        let args = match link_args(false) {
            Ok(a) => a,
            Err(e) => panic!("link_args failed: {e}"),
        };
        let tmp = std::env::temp_dir().join(format!("zz-ffi-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("tmpdir");
        let c_path = tmp.join("t.c");
        let bin_path = tmp.join("t");
        let src = format!(
            "{FFI_H}\n#include <stdio.h>\n#include <string.h>\nint main(void) {{\n    if (zz_rt_version() != {ver}ULL) return 10;\n    uint64_t live0 = zz_rt_handle_live();\n    uint64_t id = zz_rt_handle_alloc((const uint8_t *)\"regex\", 5);\n    if (id == 0) return 11;\n    if (!zz_rt_handle_tag_eq(id, (const uint8_t *)\"regex\", 5)) return 12;\n    if (zz_rt_handle_tag_eq(id, (const uint8_t *)\"uuid\", 4)) return 13;\n    if (zz_rt_handle_live() != live0 + 1) return 14;\n    if (!zz_rt_handle_drop(id)) return 15;\n    if (zz_rt_handle_tag_eq(id, (const uint8_t *)\"regex\", 5)) return 16;\n    printf(\"ffi_link_ok\\n\");\n    return 0;\n}}\n",
            ver = FFI_VERSION
        );
        std::fs::write(&c_path, &src).expect("write C");
        let cc = crate::detect_cc().expect("no C compiler");
        let mut cmd = Command::new(&cc.path);
        cmd.arg("-O1").arg("-o").arg(&bin_path).arg(&c_path);
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
