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

/// e2e_pm_offline_cold: `zz build` with unfetched dependencies fails with a
/// clear error — no network call attempted, no hang, no silent failure.
///
/// Production-readiness gate: user runs `zz build` before `zz install`,
/// must get a clear error, not a git clone hanging on network.
#[test]
fn e2e_pm_offline_cold() {
    use std::fs;

    let dir = temp_project();

    // Set ZZ_HOME to a fresh disposable dir so CAS is empty
    let zz_home = std::env::temp_dir().join(format!(
        "zz_e2e_offline_cold_{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&zz_home);

    // Create a project with a git dependency in zz.toml
    fs::write(
        dir.join("zz.toml"),
        r#"
[package]
name = "offline_test"
version = "0.1.0"

[dependencies]
some_dep = { version = "^1.0", git = "https://github.com/example/nonexistent_repo.git", rev = "main" }
"#,
    )
    .expect("write zz.toml");

    // Create zz.lock with a pinned commit (simulating a prior install)
    fs::write(
        dir.join("zz.lock"),
        r#"
version = 1

[[deps]]
name = "some_dep"
version = "^1.0"
source = "git+https://github.com/example/nonexistent_repo.git#main"
hash = ""
commit = "abc123def456789abc123def456789abc123def"
"#,
    )
    .expect("write zz.lock");

    // Create a minimal source file that imports the dependency
    fs::write(
        dir.join("hello.zz"),
        "import some_dep\nprintln(\"hello\")\n",
    )
    .expect("write source");

    // Try to build — should fail because CAS entry doesn't exist
    // Spawn with a timeout to ensure we don't hang on network
    let start = std::time::Instant::now();
    let child = Command::new(zz())
        .args(["build", "hello.zz"])
        .current_dir(&dir)
        .env("ZZ_HOME", &zz_home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn zz");

    let timeout = std::time::Duration::from_secs(10);
    let child_handle = child; // rename for clarity
    let child_thread = std::thread::spawn(move || child_handle.wait_with_output());

    // Wait with timeout
    let deadline = std::time::Instant::now() + timeout;
    let output = loop {
        if std::time::Instant::now() >= deadline {
            break None;
        }
        if child_thread.is_finished() {
            break Some(child_thread.join().expect("child thread panicked"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let elapsed = start.elapsed();

    match output {
        Some(Ok(output)) => {
            let code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            assert_ne!(
                code, 0,
                "build must fail when CAS entry is missing.\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );

            // Error should mention the missing dependency or import failure
            let combined = format!("{stdout}{stderr}");
            assert!(
                combined.contains("missing")
                    || combined.contains("not found")
                    || combined.contains("CAS")
                    || combined.contains("install")
                    || combined.contains("abc123")
                    || combined.contains("cannot read")
                    || combined.contains("No such file"),
                "error message should mention missing dep or import failure.\nstderr:\n{stderr}"
            );

            // Should NOT have attempted a git clone (would hang on network)
            assert!(
                !combined.contains("Cloning into"),
                "should not attempt git clone in offline mode.\nstderr:\n{stderr}"
            );
        }
        None => {
            panic!("zz build timed out after {timeout:?} — possible network hang");
        }
        Some(Err(e)) => {
            eprintln!("command failed: {e}");
        }
    }

    // Verify it completed quickly (< 15s) — no network hang
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "build took too long ({elapsed:?}), possible network hang"
    );

    // Clean up
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&zz_home);
}
