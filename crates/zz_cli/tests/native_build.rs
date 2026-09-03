//! CLI native-build e2e tests: `zz build`, `zz run --native`, and caching.

use std::path::Path;
use std::process::Command;

/// The compiled `zz` binary path (workspace target dir).
fn zz_bin() -> PathBuf {
    // From target/debug/deps/zz_cli-<hash>:
    //   ../../.. = target/  -> target/zz? No: bin is target/debug/zz.
    let exe = std::env::current_exe().unwrap();
    // Strip the deps/<crate>-<hash> filename.
    let deps = exe.parent().unwrap(); // .../target/debug/deps
    let debug = deps.parent().unwrap(); // .../target/debug
    debug.join("zz")
}

use std::path::PathBuf;

fn run_zz(args: &[&str]) -> (i32, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .output()
        .expect("zz binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn write_fixture(dir: &Path, name: &str, src: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, src).unwrap();
    p
}

const HELLO: &str = r#"
import std.io
func main() {
    io.println("nativetest")
    s := 0
    for i in 0..1000 {
        s = s + i
    }
    io.println(s)
}
"#;

#[test]
fn build_dev_produces_runnable_binary() {
    let dir = std::env::temp_dir().join(format!("zz-cli-test-{}", std::process::id()));
    let f = write_fixture(&dir, "app.zz", HELLO);
    let (code, out) = run_zz(&["build", f.to_str().unwrap()]);
    assert_eq!(code, 0, "build failed: {out}");
    let bin = dir.join("app");
    assert!(bin.exists(), "binary not produced");

    // Run the binary independently.
    let o = Command::new(&bin).output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(stdout, "nativetest\n499500\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_release_is_small_and_works() {
    let dir = std::env::temp_dir().join(format!("zz-cli-rel-{}", std::process::id()));
    let f = write_fixture(&dir, "rp.zz", HELLO);
    let (code, out) = run_zz(&["build", "-p", f.to_str().unwrap()]);
    assert_eq!(code, 0, "release build failed: {out}");
    let bin = dir.join("rp");
    let size = std::fs::metadata(&bin).unwrap().len();
    assert!(
        size < 2_000_000,
        "release binary too big: {size} bytes (target <= 2MB)"
    );
    let o = Command::new(&bin).output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(stdout, "nativetest\n499500\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_native_executes_and_outputs() {
    let dir = std::env::temp_dir().join(format!("zz-cli-native-{}", std::process::id()));
    let f = write_fixture(&dir, "rn.zz", HELLO);
    let (code, out) = run_zz(&["run", "--native", f.to_str().unwrap()]);
    assert_eq!(code, 0, "native run failed: {out}");
    assert_eq!(out, "nativetest\n499500\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_cache_reuses_fast_on_unchanged_source() {
    let dir = std::env::temp_dir().join(format!("zz-cli-cache-{}", std::process::id()));
    let f = write_fixture(&dir, "rc.zz", HELLO);
    // First build populates cache.
    let (code, _) = run_zz(&["build", f.to_str().unwrap()]);
    assert_eq!(code, 0);
    // Second build should be instant (cache hit) — just verify success.
    let (code2, out2) = run_zz(&["build", f.to_str().unwrap()]);
    assert_eq!(code2, 0, "cached build failed: {out2}");
    let _ = std::fs::remove_dir_all(&dir);
}
