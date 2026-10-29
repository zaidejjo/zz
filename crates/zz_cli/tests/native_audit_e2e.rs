//! Native-closure audit E2E (#265): `zz audit` on a declarative `cc`
//! plugin prints the build record and passes; source tampering, lock
//! compiler drift, and legacy hooks fail; `--allow-hooks` acknowledges
//! hooks. Tempdir pattern mirrors `c_plugin_e2e.rs`.

// `zz run` cannot dlopen plugins on non-Unix hosts (`load_vm_plugins` is a
// no-op stub there), so every assertion below is Unix-only by construction.
// The declarative AOT half is covered on Windows by `zz_pm`'s build tests.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The compiled `zz` binary (target/debug/zz).
fn zz_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    deps.parent().unwrap().join("zz")
}

fn run_zz(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("zz binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

// ---------------------------------------------------------------------------
// Declarative cc toy package (no shell, no script, no network)
// ---------------------------------------------------------------------------

const FNV_TOML: &str = r#"[package]
name = "fnv"
version = "0.1.0"

[native]
backend = "cc"
manifest = "plugin.zzi"

[native.build-cc]
sources = ["csrc/fnv.c"]
"#;

const FNV_ZZI: &str = r#"// Version: 1
// C-ABI: 1
// Plugin-version: 0.1.0

extern "C" {
    func fnv.hash(s: str) -> int
}
"#;

const FNV_C: &str = r#"#include <stdint.h>

const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;

long long fnv_hash(const char* s) {
    uint64_t h = 1469598103934665603ULL;
    if (s) {
        while (*s) {
            h ^= (unsigned char)(*s++);
            h *= 1099511628211ULL;
        }
    }
    return (long long)(h & 0x7fffffffffffffffLL);
}
"#;

const CONSUMER_TOML: &str = r#"[package]
name = "fnvuse"
version = "0.1.0"

[dependencies.fnv]
path = "../fnv"
"#;

const CONSUMER_MAIN: &str = r#"import fnv

func main() {
    who := "hello"
    println("{fnv.hash(who)}")
    println("fnv-ok")
}
"#;

// ---------------------------------------------------------------------------
// Legacy hook toy (deprecated path, gated behind --allow-hooks)
// ---------------------------------------------------------------------------

const HOOK_TOML: &str = r#"[package]
name = "hooktoy"
version = "0.1.0"

[native]
build = "build.sh"
"#;

const HOOK_ZZI: &str = r#"// Version: 1
// C-ABI: 1
// Plugin-version: 0.1.0

extern "C" {
    func hooktoy.answer() -> int
}
"#;

const HOOK_C: &str = r#"const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;

int hooktoy_answer(void) { return 42; }
"#;

const HOOK_BUILD_SH: &str = r#"#!/bin/sh
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
mkdir -p "$BUILD_DIR"
cc -c "$SCRIPT_DIR/csrc/hooktoy.c" -o "$BUILD_DIR/hooktoy.o" -fPIC
cc -shared -fPIC "$BUILD_DIR/hooktoy.o" -o "$BUILD_DIR/libhooktoy_native.so"
: > "$BUILD_DIR/ldflags.txt"
"#;

const HOOK_CONSUMER_TOML: &str = r#"[package]
name = "hookuse"
version = "0.1.0"

[dependencies.hooktoy]
path = "../hooktoy"
"#;

/// Layout: <tmp>/fnv + <tmp>/use, <tmp>/hooktoy + <tmp>/hookuse.
fn scaffold(dir: &Path) {
    let pkg = dir.join("fnv");
    write(&pkg, "zz.toml", FNV_TOML);
    write(&pkg, "plugin.zzi", FNV_ZZI);
    write(&pkg, "csrc/fnv.c", FNV_C);

    let consumer = dir.join("use");
    write(&consumer, "zz.toml", CONSUMER_TOML);
    write(&consumer, "src/main.zz", CONSUMER_MAIN);

    let hook = dir.join("hooktoy");
    write(&hook, "zz.toml", HOOK_TOML);
    write(&hook, "plugin.zzi", HOOK_ZZI);
    write(&hook, "csrc/hooktoy.c", HOOK_C);
    write(&hook, "build.sh", HOOK_BUILD_SH);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sh = hook.join("build.sh");
        let mut perms = std::fs::metadata(&sh).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&sh, perms).unwrap();
    }

    let hook_consumer = dir.join("hookuse");
    write(&hook_consumer, "zz.toml", HOOK_CONSUMER_TOML);
}

#[test]
fn native_audit_cc_passes_and_tamper_fails() {
    let dir = std::env::temp_dir().join(format!("zz-audit-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    scaffold(&dir);
    let consumer = dir.join("use");

    let (code, _, stderr) = run_zz(&consumer, &["install"]);
    assert_eq!(code, 0, "install failed: {stderr}");

    // The plugin actually works end to end.
    let (code, stdout, stderr) = run_zz(&consumer, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(stdout.contains("fnv-ok"), "unexpected output: {stdout}");

    // Clean audit: prints the native closure, passes. Toys carry no
    // license metadata, so a license warning is expected.
    let (code, stdout, stderr) = run_zz(&consumer, &["audit"]);
    let combined = format!("{stdout}\n{stderr}");
    assert_eq!(code, 0, "audit failed: {combined}");
    assert!(
        combined.contains("backend=cc"),
        "native closure missing: {combined}"
    );
    assert!(
        combined.contains("no license metadata"),
        "license warning missing: {combined}"
    );

    // Tamper with a csrc file: audit fails naming the dep.
    let csrc = dir.join("fnv/csrc/fnv.c");
    let orig = std::fs::read_to_string(&csrc).unwrap();
    std::fs::write(&csrc, format!("{orig}\n// tampered\n")).unwrap();
    let (code, _, stderr) = run_zz(&consumer, &["audit"]);
    let (code2, stdout2, _) = run_zz(&consumer, &["audit"]);
    let combined = format!("{stdout2}\n{stderr}");
    assert_ne!(code, 0, "audit passed despite tampering");
    assert_eq!(code2, code, "audit exit unstable");
    assert!(
        combined.contains("fnv") && combined.contains("content changed"),
        "tamper diff missing: {combined}"
    );
    std::fs::write(&csrc, orig).unwrap();

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn native_audit_hook_fails_without_flag() {
    let dir = std::env::temp_dir().join(format!("zz-audit-hook-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    scaffold(&dir);
    let consumer = dir.join("hookuse");

    let (code, _, stderr) = run_zz(&consumer, &["install", "--allow-hooks"]);
    assert_eq!(code, 0, "install failed: {stderr}");

    // Legacy hook in the closure: audit refuses without acknowledgement.
    let (code, _, stderr) = run_zz(&consumer, &["audit"]);
    assert_ne!(code, 0, "audit passed with unacknowledged hook");
    assert!(
        stderr.contains("hooktoy") && stderr.contains("--allow-hooks"),
        "hook refusal missing: {stderr}"
    );

    // Acknowledged: passes (with a warning), hook still runs the code.
    let (code, _, stderr) = run_zz(&consumer, &["audit", "--allow-hooks"]);
    let combined = stderr;
    assert_eq!(code, 0, "acknowledged audit failed: {combined}");
    assert!(
        combined.contains("acknowledged via --allow-hooks"),
        "hook warning missing: {combined}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
