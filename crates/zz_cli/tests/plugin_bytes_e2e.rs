//! Bytes-view plugin E2E (#234 item 1): a declarative `cc` plugin taking
//! a borrowed `bytes` param. Proves ptr+len marshaling with no copy and
//! no NUL termination (interior NUL bytes are counted), on both engines.
//! Tempdir pattern mirrors `c_plugin_e2e.rs`.

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
// Declarative cc toy package with a bytes-view function
// ---------------------------------------------------------------------------

const PKG_TOML: &str = r#"[package]
name = "bytehash"
version = "0.1.0"

[native]
backend = "cc"
manifest = "plugin.zzi"

[native.build-cc]
sources = ["csrc/bytehash.c"]
"#;

const PKG_ZZI: &str = r#"// Version: 1
// C-ABI: 1
// Plugin-version: 0.1.0

extern "C" {
    func bytehash.sum(buf: bytes) -> int
}
"#;

const PKG_C: &str = r#"#include <stddef.h>
#include <stdint.h>

const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;

long long bytehash_sum(const uint8_t *ptr, size_t len) {
    long long total = 0;
    for (size_t i = 0; i < len; i++) {
        total += ptr[i];
    }
    return total;
}
"#;

const CONSUMER_TOML: &str = r#"[package]
name = "byteuse"
version = "0.1.0"

[dependencies.bytehash]
path = "../bytehash"
"#;

const CONSUMER_MAIN: &str = r#"import bytehash
import std.bytes

func main() -> Result<int, str> {
    // Interior NUL (0) and 255: a strlen-based marshaling would stop at
    // index 1 and report 1; the borrowed view reports 1+0+2+250 = 253.
    b := bytes.finish([1, 0, 2, 250])?
    s := bytehash.sum(b)
    println("sum={s}")
    println("bytehash-ok")
    .ok(0)
}
"#;

const CONSUMER_MISMATCH: &str = r#"import bytehash

func main() {
    println("{bytehash.sum(42)}")
}
"#;

/// Layout: <tmp>/bytehash (package) + <tmp>/use (consumer with path dep).
fn scaffold(dir: &Path) {
    let pkg = dir.join("bytehash");
    write(&pkg, "zz.toml", PKG_TOML);
    write(&pkg, "plugin.zzi", PKG_ZZI);
    write(&pkg, "csrc/bytehash.c", PKG_C);

    let consumer = dir.join("use");
    write(&consumer, "zz.toml", CONSUMER_TOML);
    write(&consumer, "src/main.zz", CONSUMER_MAIN);
}

#[test]
fn bytes_view_vm_and_aot_agree() {
    let dir = std::env::temp_dir().join(format!("zz-bytehash-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    scaffold(&dir);
    let consumer = dir.join("use");

    let (code, _, stderr) = run_zz(&consumer, &["install"]);
    assert_eq!(code, 0, "install failed: {stderr}");

    // VM path: dlopen + borrowed view.
    let (code, stdout, stderr) = run_zz(&consumer, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(
        stdout.contains("sum=253") && stdout.contains("bytehash-ok"),
        "unexpected VM output: {stdout}"
    );

    // AOT path: same source, same result.
    let (code, _, stderr) = run_zz(&consumer, &["build", "src/main.zz"]);
    assert_eq!(code, 0, "build failed: {stderr}");
    let bin = consumer.join("bin/byteuse");
    assert!(bin.is_file(), "project binary missing");
    let out = Command::new(&bin).output().expect("binary should run");
    let bin_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        bin_stdout.contains("sum=253") && bin_stdout.contains("bytehash-ok"),
        "unexpected AOT output: {bin_stdout}"
    );

    // Mismatched value fails loudly on the VM (names the expectation).
    write(&consumer, "src/mismatch.zz", CONSUMER_MISMATCH);
    let (code, _, stderr) = run_zz(&consumer, &["run", "src/mismatch.zz"]);
    assert_ne!(code, 0, "mismatched arg unexpectedly passed");
    assert!(
        stderr.contains("expected `bytes`"),
        "loud mismatch missing: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
