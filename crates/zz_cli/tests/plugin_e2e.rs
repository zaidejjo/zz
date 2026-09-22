//! Plugin E2E (Milestones 3+4): a toy native package consumed via
//! `import toy`, exercised identically through `zz build` (AOT) and
//! `zz run` (VM dlopen). Covers dotted ZZ names, explicit C-symbol
//! overrides (`= "..."`), str params, and ABI-mismatch refusal.

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

fn zz_runtime_path() -> String {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .join("zz_runtime")
        .display()
        .to_string()
}

// ---------------------------------------------------------------------------
// Toy package sources (written into a tempdir by each test)
// ---------------------------------------------------------------------------

const TOY_TOML: &str = r#"[package]
name = "toy"
version = "0.1.0"

[native]
build = "build.sh"
"#;

const TOY_ZZI: &str = r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func toy.add(a: int, b: int) -> int = "toy_add_impl";
    func toy.starts_with(s: str, prefix: str) -> int
    func toy.noop()
}
"#;

const TOY_C: &str = r#"#include <string.h>

int toy_add_impl(int a, int b) { return a + b; }

int toy_starts_with(const char* s, const char* prefix) {
    if (!s || !prefix) return 0;
    size_t n = strlen(prefix);
    return strncmp(s, prefix, n) == 0 ? 1 : 0;
}

void toy_noop(void) {}
"#;

const TOY_BUILD_SH: &str = r#"#!/bin/sh
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
NATIVE_DIR="$SCRIPT_DIR/native"
mkdir -p "$BUILD_DIR"
cc -c "$SCRIPT_DIR/csrc/toy.c" -o "$BUILD_DIR/toy.o" -Wall -Wextra -fPIC
cd "$NATIVE_DIR"
cargo rustc --release --lib --crate-type cdylib -- \
	-C link-args=-Wl,--exclude-libs,ALL \
	-C link-args=-Wl,-z,lazy 2>&1 | tail -1
SO_FILE=$(find "$NATIVE_DIR/target/release/deps" -maxdepth 1 -name "libtoy_native.so" 2>/dev/null | head -1)
if [ -n "$SO_FILE" ]; then
	cp "$SO_FILE" "$BUILD_DIR/"
fi
cargo build --release 2>&1 | tail -1
A_FILE="$NATIVE_DIR/target/release/libtoy_native.a"
if [ -f "$A_FILE" ]; then
	cp "$A_FILE" "$BUILD_DIR/"
fi
: > "$BUILD_DIR/ldflags.txt"
"#;

fn toy_cargo_toml(zz_runtime: &str) -> String {
    format!(
        r#"[package]
name = "toy_native"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "staticlib"]

[dependencies]
zz_runtime = {{ path = "{zz_runtime}" }}

[build-dependencies]
cc = "1"
"#
    )
}

const TOY_BUILD_RS: &str = r#"fn main() {
    cc::Build::new().file("../csrc/toy.c").compile("toy");
}
"#;

const TOY_LIB_RS: &str = r#"use std::ffi::CString;

#[no_mangle]
pub static ZZ_PLUGIN_ABI_VERSION: u32 = 1;

extern "C" {
    fn toy_add_impl(a: i32, b: i32) -> i32;
    fn toy_starts_with(s: *const i8, prefix: *const i8) -> i32;
    fn toy_noop();
}

fn native_add(
    _interp: &mut zz_runtime::eval::Interp,
    args: &mut Vec<zz_runtime::Value>,
    _span: zz_runtime::Span,
) -> Result<zz_runtime::Value, zz_runtime::EvalError> {
    let (a, b) = match (&args[0], &args[1]) {
        (zz_runtime::Value::Int(a), zz_runtime::Value::Int(b)) => (*a as i32, *b as i32),
        _ => {
            return Err(zz_runtime::EvalError::new(
                "toy.add expects (int, int)",
                zz_runtime::Span::new(0, 0),
            ))
        }
    };
    Ok(zz_runtime::Value::Int(unsafe { toy_add_impl(a, b) } as i64))
}

fn native_starts_with(
    _interp: &mut zz_runtime::eval::Interp,
    args: &mut Vec<zz_runtime::Value>,
    _span: zz_runtime::Span,
) -> Result<zz_runtime::Value, zz_runtime::EvalError> {
    let (s, prefix) = match (&args[0], &args[1]) {
        (zz_runtime::Value::Str(s), zz_runtime::Value::Str(p)) => (s.as_str(), p.as_str()),
        _ => {
            return Err(zz_runtime::EvalError::new(
                "toy.starts_with expects (str, str)",
                zz_runtime::Span::new(0, 0),
            ))
        }
    };
    let cs = CString::new(s).map_err(|_| zz_runtime::EvalError::new("interior NUL", zz_runtime::Span::new(0, 0)))?;
    let cp = CString::new(prefix).map_err(|_| zz_runtime::EvalError::new("interior NUL", zz_runtime::Span::new(0, 0)))?;
    Ok(zz_runtime::Value::Int(
        unsafe { toy_starts_with(cs.as_ptr(), cp.as_ptr()) } as i64,
    ))
}

fn native_noop(
    _interp: &mut zz_runtime::eval::Interp,
    args: &mut Vec<zz_runtime::Value>,
    _span: zz_runtime::Span,
) -> Result<zz_runtime::Value, zz_runtime::EvalError> {
    args.clear();
    unsafe { toy_noop() };
    Ok(zz_runtime::Value::Unit)
}

#[allow(improper_ctypes_definitions)]
type RegisterCallback = extern "C" fn(name: *const i8, arity: usize, f: zz_runtime::NativeFn);

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn zz_plugin_register(callback: RegisterCallback) {
    let name = CString::new("toy.add").unwrap();
    callback(name.as_ptr(), 2, native_add);
    let name = CString::new("toy.starts_with").unwrap();
    callback(name.as_ptr(), 2, native_starts_with);
    let name = CString::new("toy.noop").unwrap();
    callback(name.as_ptr(), 0, native_noop);
}
"#;

const CONSUMER_TOML: &str = r#"[package]
name = "toyuse"
version = "0.1.0"

[dependencies.toy]
path = "../toy"
"#;

const CONSUMER_MAIN: &str = r#"import toy

func main() {
    r := toy.add(40, 2)
    println("add: {r}")
    s := toy.starts_with("hello world", "hello")
    println("sw: {s}")
    s2 := toy.starts_with("hello world", "world")
    println("sw2: {s2}")
    toy.noop()
    println("toy-ok")
}
"#;

const EXPECTED: &str = "add: 42\nsw: 1\nsw2: 0\ntoy-ok\n";

/// Layout: <tmp>/toy (package) + <tmp>/use (consumer with path dep).
fn scaffold(dir: &Path) {
    let toy = dir.join("toy");
    write(&toy, "zz.toml", TOY_TOML);
    write(&toy, "plugin.zzi", TOY_ZZI);
    write(&toy, "csrc/toy.c", TOY_C);
    write(&toy, "build.sh", TOY_BUILD_SH);
    write(
        &toy,
        "native/Cargo.toml",
        &toy_cargo_toml(&zz_runtime_path()),
    );
    write(&toy, "native/build.rs", TOY_BUILD_RS);
    write(&toy, "native/src/lib.rs", TOY_LIB_RS);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sh = toy.join("build.sh");
        let mut perms = std::fs::metadata(&sh).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&sh, perms).unwrap();
    }

    let consumer = dir.join("use");
    write(&consumer, "zz.toml", CONSUMER_TOML);
    write(&consumer, "src/main.zz", CONSUMER_MAIN);
}

#[test]
fn toy_plugin_aot_and_vm_agree() {
    let dir = std::env::temp_dir().join(format!("zz-toy-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    scaffold(&dir);
    let consumer = dir.join("use");

    let (code, _, stderr) = run_zz(&consumer, &["install"]);
    assert_eq!(code, 0, "install failed: {stderr}");

    // AOT path.
    let (code, _, stderr) = run_zz(&consumer, &["build", "src/main.zz"]);
    assert_eq!(code, 0, "build failed: {stderr}");
    let bin = consumer.join("src/bin/main");
    let out = Command::new(&bin).output().expect("binary should run");
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);

    // VM path: same source, same result.
    let (code, stdout, stderr) = run_zz(&consumer, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "run failed: {stderr}");
    assert_eq!(stdout, EXPECTED);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// ABI mismatch: a .so with a wrong version stamp must be refused cleanly.
// ---------------------------------------------------------------------------

const BAD_ZZI: &str = r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func bad.ping() -> int
}
"#;

const BAD_TOML: &str = r#"[package]
name = "bad"
version = "0.1.0"

[native]
build = "build.sh"
"#;

const BAD_C: &str = r#"#include <stdint.h>

uint32_t ZZ_PLUGIN_ABI_VERSION = 999;

typedef void (*cb_t)(const char*, unsigned long, void*);
static cb_t saved;

void zz_plugin_register(cb_t cb) { saved = cb; (void)saved; }

int bad_ping(void) { return 1; }
"#;

const BAD_BUILD_SH: &str = r#"#!/bin/sh
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
mkdir -p "$BUILD_DIR"
cc -shared -fPIC "$SCRIPT_DIR/csrc/bad.c" -o "$BUILD_DIR/libbad_native.so"
: > "$BUILD_DIR/ldflags.txt"
"#;

const BAD_CONSUMER_TOML: &str = r#"[package]
name = "baduse"
version = "0.1.0"

[dependencies.bad]
path = "../bad"
"#;

const BAD_CONSUMER_MAIN: &str = r#"import bad

func main() {
    r := bad.ping()
    println("ping: {r}")
}
"#;

#[test]
fn toy_plugin_abi_mismatch_refused() {
    let dir = std::env::temp_dir().join(format!("zz-bad-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bad = dir.join("bad");
    write(&bad, "zz.toml", BAD_TOML);
    write(&bad, "plugin.zzi", BAD_ZZI);
    write(&bad, "csrc/bad.c", BAD_C);
    write(&bad, "build.sh", BAD_BUILD_SH);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sh = bad.join("build.sh");
        let mut perms = std::fs::metadata(&sh).unwrap().permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(&sh, perms).unwrap();
    }
    let consumer = dir.join("use");
    write(&consumer, "zz.toml", BAD_CONSUMER_TOML);
    write(&consumer, "src/main.zz", BAD_CONSUMER_MAIN);

    let (code, _, stderr) = run_zz(&consumer, &["install"]);
    assert_eq!(code, 0, "install failed: {stderr}");

    // Populate build/ by invoking the hook directly (`zz run` never
    // builds; it only loads what the hook produced).
    let hook = Command::new("sh")
        .arg("build.sh")
        .current_dir(&bad)
        .output()
        .expect("hook should run");
    assert!(
        hook.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&hook.stderr)
    );
    assert!(bad.join("build/libbad_native.so").exists());

    // VM must refuse the mismatched library with a clear error — exit
    // non-zero, mention the version mismatch, and never crash (no
    // SIGSEGV/SIGABRT exit codes).
    let (code, _, stderr) = run_zz(&consumer, &["run", "src/main.zz"]);
    assert_ne!(code, 0, "mismatched ABI must fail");
    assert!(
        !matches!(code, 139 | 134),
        "mismatched ABI must not crash (exit {code})"
    );
    assert!(
        stderr.contains("ABI version mismatch"),
        "expected version-mismatch refusal, got: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
