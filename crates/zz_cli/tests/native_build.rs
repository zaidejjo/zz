//! CLI native-build e2e tests: `zz build`, `zz run --native`, and caching.
mod support;

use std::path::Path;
use std::process::Command;

/// The compiled `zz` binary path (workspace target dir).
fn zz_bin() -> PathBuf {
    // `CARGO_BIN_EXE_zz` is the built binary for this test run and
    // carries the platform executable suffix (`zz.exe` on Windows).
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

use std::path::PathBuf;

fn run_zz_in(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .current_dir(dir)
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
func main() {
    println("nativetest")
    s := 0
    for i in 0..1000 {
        s = s + i
    }
    println(s)
}
"#;

/// Skip helper: every test in this file needs the native backend, which is
/// unsupported with `ZZ_SKIP_NATIVE=1` (Windows CI — POSIX C runtime, no FFI
/// link). Returns false after logging so failures stay visible as skips.
fn require_native() -> bool {
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return false;
    }
    true
}

#[test]
fn build_dev_produces_runnable_binary() {
    if !require_native() {
        return;
    }
    // Default build is a static self-contained binary in CWD.
    let dir = std::env::temp_dir().join(format!("zz-cli-test-{}", std::process::id()));
    let f = write_fixture(&dir, "app.zz", HELLO);
    let (code, out) = run_zz_in(&dir, &["build", f.to_str().unwrap()]);
    assert_eq!(code, 0, "build failed: {out}");
    let bin = support::exe_in(&dir, "app");
    assert!(bin.exists(), "binary not produced");

    // Run the binary independently.
    let o = Command::new(&bin).output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(stdout, "nativetest\n499500\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_release_is_small_and_works() {
    if !require_native() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("zz-cli-rel-{}", std::process::id()));
    let f = write_fixture(&dir, "rp.zz", HELLO);
    let (code, out) = run_zz_in(&dir, &["build", "-p", f.to_str().unwrap()]);
    assert_eq!(code, 0, "release build failed: {out}");
    let bin = support::exe_in(&dir, "rp");
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
    if !require_native() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("zz-cli-native-{}", std::process::id()));
    let f = write_fixture(&dir, "rn.zz", HELLO);
    let (code, out) = run_zz_in(&dir, &["run", "--native", f.to_str().unwrap()]);
    assert_eq!(code, 0, "native run failed: {out}");
    assert_eq!(out, "nativetest\n499500\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_cache_reuses_fast_on_unchanged_source() {
    if !require_native() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("zz-cli-cache-{}", std::process::id()));
    let f = write_fixture(&dir, "rc.zz", HELLO);
    // First build populates cache (release; dev builds skip native compile).
    let (code, _) = run_zz_in(&dir, &["build", "-p", f.to_str().unwrap()]);
    assert_eq!(code, 0);
    // Second build should be instant (cache hit) — just verify success.
    let (code2, out2) = run_zz_in(&dir, &["build", "-p", f.to_str().unwrap()]);
    assert_eq!(code2, 0, "cached build failed: {out2}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run `zz build --release --verbose`, returning (exit, stdout, stderr).
/// `--verbose` prints `zz: timing units_cache=<hits>/<total>` to stderr,
/// which is how we observe per-unit object-cache reuse.
fn run_zz_release_verbose(dir: &Path, main: &Path) -> (i32, String, String) {
    let out = Command::new(zz_bin())
        .args(["build", "--release", "--verbose", main.to_str().unwrap()])
        .current_dir(dir)
        .output()
        .expect("zz binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Parse `zz: timing units_cache=<hits>/<total> hits` from stderr.
fn parse_units_cache(stderr: &str) -> (usize, usize) {
    let needle = "units_cache=";
    let pos = stderr
        .find(needle)
        .expect("units_cache line missing from --verbose stderr");
    let rest = &stderr[pos + needle.len()..];
    let end = rest.find(' ').unwrap_or(rest.len());
    let pair = &rest[..end];
    let (hits, total) = pair
        .split_once('/')
        .expect("units_cache should be hits/total");
    (
        hits.parse::<usize>().expect("hits should be a number"),
        total.parse::<usize>().expect("total should be a number"),
    )
}

/// A package-importing project reuses cached stdlib/package units when only
/// project code changes between builds. This is the CLI-level proof of
/// step 5: project edits never invalidate package/stdlib unit objects.
#[test]
fn build_release_reuses_units_on_project_edit() {
    if !require_native() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("zz-cli-reuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("zz.toml"),
        "[package]\nname = \"reuse\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let main = dir.join("main.zz");

    // Version 1: populate the unit object cache.
    std::fs::write(
        &main,
        "import std.str\nfunc main() { println(str.is_empty(\"\")) }\n",
    )
    .unwrap();
    let (code, _, _) = run_zz_release_verbose(&dir, &main);
    assert_eq!(code, 0, "version 1 build failed");

    // Version 2: edit only project code (extra call in main). Source
    // changed → binary-level cache miss → codegen runs → `units_cache`
    // printed to stderr. Stdlib units must be hits; project unit a miss.
    std::fs::write(
        &main,
        "import std.str\nfunc main() { println(str.is_empty(\"\")); println(str.is_empty(\"x\")) }\n",
    )
    .unwrap();
    let (code, out, err) = run_zz_release_verbose(&dir, &main);
    assert_eq!(
        code, 0,
        "version 2 build failed:\nstdout:{out}\nstderr:{err}"
    );
    // Version 2's source differs from version 1, so the binary-level cache
    // misses and codegen runs — `units_cache` is printed to stderr. Stdlib
    // units (`str`) are unchanged between versions and must be cache hits.
    let (hits, total) = parse_units_cache(&err);
    assert!(
        total > 0,
        "should have at least one unit, got ({hits}/{total}) in {err}"
    );
    assert!(
        hits > 0,
        "project edit should reuse cached stdlib units, got ({hits}/{total}) in {err}"
    );

    // Binary still runs correctly (named after the package `reuse`,
    // placed in the project `bin/` dir).
    let bin = dir.join("bin").join("reuse");
    let o = Command::new(&bin).output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.last().copied().unwrap_or(""),
        "false",
        "unexpected output: {stdout:?}"
    );
    assert!(
        stdout.contains("true"),
        "expected 'true' in output: {stdout:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
