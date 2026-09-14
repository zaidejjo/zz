//! Build-system integration tests for the single-Clang-backend `zz build`.
//!
//! `zz build` always produces a native binary: default flags are `-O0 -g`
//! (fast debug), `-p` upgrades to `-O3 -flto=thin`.
//!
//! - dev default (`zz build`): builds `bin/<stem>`, executes it, checks
//!   output (requires Clang on PATH).
//! - without Clang (dev or release): emits `bin/app.c` + `build.sh`/
//!   `build.bat`, exits 1 with the no-clang error.
//! - guard rails: `--pgo` + foreign `--target` and `--static` on macOS
//!   triples fail with the exact CLI-contract errors.
//! - removed `--dev` flag is rejected with a hint.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const PROG: &str = "println(\"build_ok\")\n";

/// Fresh temp project dir with a minimal `hello.zz`.
fn temp_project() -> PathBuf {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("zz-build-e2e-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmpdir");
    std::fs::write(dir.join("hello.zz"), PROG).expect("fixture");
    dir
}

fn zz() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn run(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(zz());
    cmd.args(args).current_dir(dir);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("exec zz");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn dev_default_builds_native_binary() {
    let dir = temp_project();
    let (code, stdout, stderr) = run(&dir, &["build", "hello.zz"], &[]);
    if !stdout.contains("(dev,") && stderr.contains("no clang found") {
        eprintln!("SKIP: no clang on PATH, cannot run dev happy path");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(
        code, 0,
        "dev build must pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("(dev,"), "dev marker missing:\n{stdout}");
    let bin = dir.join("bin/hello");
    assert!(bin.is_file(), "bin/hello missing");
    let out = Command::new(&bin).output().expect("run binary");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "build_ok\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dev_without_clang_emits_c_and_scripts() {
    let dir = temp_project();
    // Dev builds need Clang too — same missing-toolchain path as release.
    let (code, _stdout, stderr) = run(&dir, &["build", "hello.zz"], &[("ZZ_TEST_HIDE_CLANG", "1")]);
    assert_eq!(code, 1, "dev without clang must fail.\nstderr:\n{stderr}");
    assert!(
        stderr.contains("no clang found"),
        "no-clang error missing:\n{stderr}"
    );
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    assert!(dir.join("bin/build.sh").is_file(), "bin/build.sh missing");
    assert!(dir.join("bin/build.bat").is_file(), "bin/build.bat missing");
    assert!(!dir.join("bin/hello").is_file(), "no binary should exist");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn release_without_clang_emits_c_and_scripts() {
    let dir = temp_project();
    let (code, _stdout, stderr) = run(
        &dir,
        &["build", "-p", "hello.zz"],
        &[("ZZ_TEST_HIDE_CLANG", "1")],
    );
    assert_eq!(
        code, 1,
        "release without clang must fail.\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("no clang found"),
        "no-clang error missing:\n{stderr}"
    );
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    assert!(dir.join("bin/build.sh").is_file(), "bin/build.sh missing");
    assert!(dir.join("bin/build.bat").is_file(), "bin/build.bat missing");
    assert!(!dir.join("bin/hello").is_file(), "no binary should exist");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pgo_cross_target_rejected() {
    let dir = temp_project();
    let (code, _stdout, stderr) = run(
        &dir,
        &[
            "build",
            "-p",
            "--pgo",
            "--target",
            "aarch64-unknown-linux-gnu",
            "hello.zz",
        ],
        &[],
    );
    assert_eq!(code, 1, "pgo+cross must fail.\nstderr:\n{stderr}");
    assert!(
        stderr.contains("Error: --pgo requires a native build target"),
        "exact pgo error missing:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn static_macos_target_rejected() {
    let dir = temp_project();
    let (code, _stdout, stderr) = run(
        &dir,
        &[
            "build",
            "-p",
            "--static",
            "--target",
            "x86_64-apple-darwin",
            "hello.zz",
        ],
        &[],
    );
    assert_eq!(code, 1, "static+macos must fail.\nstderr:\n{stderr}");
    assert!(
        stderr.contains("Error: Static binaries are not supported on macOS targets"),
        "exact static error missing:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn removed_dev_flag_is_rejected() {
    let dir = temp_project();
    let (code, _stdout, stderr) = run(&dir, &["build", "--dev", "hello.zz"], &[]);
    assert_eq!(code, 1, "removed --dev must fail.\nstderr:\n{stderr}");
    assert!(
        stderr.contains("`--dev` was removed"),
        "removal error missing:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn release_native_builds_and_runs() {
    let dir = temp_project();
    let (code, stdout, stderr) = run(&dir, &["build", "-p", "hello.zz"], &[]);
    if !stdout.contains("release") && stderr.contains("no clang found") {
        eprintln!("SKIP: no clang on PATH, cannot run release happy path");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(
        code, 0,
        "release build must pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let bin = dir.join("bin/hello");
    assert!(bin.is_file(), "bin/hello missing");
    let out = Command::new(&bin).output().expect("run binary");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "build_ok\n");
    let _ = std::fs::remove_dir_all(&dir);
}
