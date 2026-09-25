//! C-only plugin E2E: a toy native package with NO Rust shim, consumed via
//! `import ctoy` through `zz run` (VM direct-dlsym) and `zz build` (AOT).
//! Covers dotted names, explicit C-symbol overrides (`= "..."`), str and
//! float params, and void returns. The hook is `cc` + `sh` only — this test
//! fails if the C path ever shells out to cargo.

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
// C-only toy package (written into a tempdir by the test)
// ---------------------------------------------------------------------------

const TOY_TOML: &str = r#"[package]
name = "ctoy"
version = "0.1.0"

[native]
build = "build.sh"
"#;

const TOY_ZZI: &str = r#"// Version: 1
// C-ABI: 1
// Plugin-version: 0.1.0

extern "C" {
    func ctoy.add(a: int, b: int) -> int = "ctoy_add_impl";
    func ctoy.starts_with(s: str, prefix: str) -> int
    func ctoy.scale(h: int, f: float) -> int
    func ctoy.noop()
}
"#;

const TOY_C: &str = r#"#include <string.h>

const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;

int ctoy_add_impl(int a, int b) { return a + b; }

int ctoy_starts_with(const char* s, const char* prefix) {
    if (!s || !prefix) return 0;
    size_t n = strlen(prefix);
    return strncmp(s, prefix, n) == 0 ? 1 : 0;
}

int ctoy_scale(long long h, double f) { return (int)(h * f); }

void ctoy_noop(void) {}
"#;

// NOTE: cc + sh only. No cargo, no rustc — by design.
const TOY_BUILD_SH: &str = r#"#!/bin/sh
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
mkdir -p "$BUILD_DIR"
cc -c "$SCRIPT_DIR/csrc/ctoy.c" -o "$BUILD_DIR/ctoy.o" -Wall -Wextra -fPIC
cc -shared -fPIC "$BUILD_DIR/ctoy.o" -o "$BUILD_DIR/libctoy_native.so"
: > "$BUILD_DIR/ldflags.txt"
"#;

const CONSUMER_TOML: &str = r#"[package]
name = "ctoyuse"
version = "0.1.0"

[dependencies.ctoy]
path = "../ctoy"
"#;

const CONSUMER_MAIN: &str = r#"import ctoy

func main() {
    r := ctoy.add(40, 2)
    println("add: {r}")
    s := ctoy.starts_with("hello world", "hello")
    println("sw: {s}")
    s2 := ctoy.starts_with("hello world", "world")
    println("sw2: {s2}")
    v := ctoy.scale(21, 2.0)
    println("scale: {v}")
    ctoy.noop()
    println("ctoy-ok")
}
"#;

const EXPECTED: &str = "add: 42\nsw: 1\nsw2: 0\nscale: 42\nctoy-ok\n";

const PLUGIN_TEST: &str = r#"import ctoy

@test
func test_ctoy_add() {
    assert_eq(ctoy.add(40, 2), 42)
}

@test
func test_ctoy_starts_with() {
    assert_eq(ctoy.starts_with("hello world", "hello"), 1)
    assert_eq(ctoy.starts_with("hello world", "world"), 0)
}

@test
func test_ctoy_scale() {
    assert_eq(ctoy.scale(21, 2.0), 42)
}
"#;

/// Layout: <tmp>/ctoy (package) + <tmp>/use (consumer with path dep).
fn scaffold(dir: &Path) {
    let toy = dir.join("ctoy");
    write(&toy, "zz.toml", TOY_TOML);
    write(&toy, "plugin.zzi", TOY_ZZI);
    write(&toy, "csrc/ctoy.c", TOY_C);
    write(&toy, "build.sh", TOY_BUILD_SH);
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
fn c_plugin_vm_and_aot_agree() {
    let dir = std::env::temp_dir().join(format!("zz-ctoy-e2e-{}", std::process::id()));
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

    // VM path: same source, same result — via direct dlsym, no Rust shim.
    let (code, stdout, stderr) = run_zz(&consumer, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "run failed: {stderr}");
    assert_eq!(stdout, EXPECTED);

    // `zz test` phase: @test functions calling C natives — the runner
    // dlopens build/*.so exactly like `zz run`.
    write(&consumer, "tests/ctoy_test.zz", PLUGIN_TEST);
    let (code, stdout, stderr) = run_zz(&consumer, &["test"]);
    assert_eq!(code, 0, "test failed:\n{stdout}\n{stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}
