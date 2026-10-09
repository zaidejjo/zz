//! Build-output redesign regressions: standalone vs project destinations.
//!
//! Discovery precedence (authoritative, single writer — no legacy
//! duplicates):
//! - no argument: project discovered from CWD; outside a project the
//!   file argument stays required.
//! - explicit source path: the file's owning project (nearest ancestor
//!   holding `zz.toml`) decides the output directory, even when invoked
//!   from another directory.
//! - standalone: `./<stem>` in CWD (never `./bin/`).
//! - project: `<root>/bin/<package-name>` for the conventional entry
//!   (`src/main.zz`, then `main.zz`), `<root>/bin/<stem>` for explicit
//!   non-entry files (never `src/bin/`).
//! - bare `-o`: renames inside the resolved directory; path-like `-o`
//!   is used as-is relative to CWD.
//! - `zz run` (VM) leaves no build artifacts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const PROG: &str = "println(\"output_ok\")\n";
const PKG: &str = "demopkg";

fn zz() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn fresh_root(tag: &str) -> PathBuf {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zz-build-output-{tag}-{}-{uniq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmpdir");
    dir
}

/// Standalone sandbox: one `.zz` file, no `zz.toml` anywhere above.
fn standalone_dir() -> (PathBuf, PathBuf) {
    let dir = fresh_root("standalone");
    let src = dir.join("hello.zz");
    std::fs::write(&src, PROG).expect("fixture");
    (dir, src)
}

/// Project sandbox: `zz.toml` + `src/main.zz`.
fn project_dir() -> PathBuf {
    let dir = fresh_root("project");
    std::fs::write(
        dir.join("zz.toml"),
        format!("[package]\nname = \"{PKG}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("manifest");
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).expect("src");
    std::fs::write(src_dir.join("main.zz"), PROG).expect("entry");
    dir
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

fn no_clang() -> [(&'static str, &'static str); 1] {
    [("ZZ_TEST_HIDE_CLANG", "1")]
}

// --- Argument / discovery errors (no toolchain needed) ---

#[test]
fn build_without_arg_outside_project_requires_file() {
    let (dir, _) = standalone_dir();
    let (code, _, stderr) = run(&dir, &["build"], &[]);
    assert_ne!(code, 0, "bare build outside a project must fail");
    assert!(
        stderr.contains("missing file argument"),
        "needs missing-file error, got:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_without_entry_in_project_errors() {
    let dir = fresh_root("noentry");
    std::fs::write(
        dir.join("zz.toml"),
        "[package]\nname = \"empty\"\nversion = \"0.1.0\"\n",
    )
    .expect("manifest");
    let (code, _, stderr) = run(&dir, &["build"], &[]);
    assert_ne!(code, 0, "bare build without an entry must fail");
    assert!(
        stderr.contains("no entry file"),
        "needs no-entry error, got:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_without_arg_outside_project_requires_file() {
    let (dir, _) = standalone_dir();
    let (code, _, stderr) = run(&dir, &["run"], &[]);
    assert_ne!(code, 0, "bare run outside a project must fail");
    assert!(
        stderr.contains("missing file argument"),
        "needs missing-file error, got:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn profile_without_arg_outside_project_requires_file() {
    let (dir, _) = standalone_dir();
    let (code, _, stderr) = run(&dir, &["profile"], &[]);
    assert_ne!(code, 0, "bare profile outside a project must fail");
    assert!(
        stderr.contains("missing file argument"),
        "needs missing-file error, got:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- No-clang fallback destinations (deterministic everywhere) ---

#[test]
fn no_clang_standalone_emits_beside_binary_in_cwd() {
    let (dir, _) = standalone_dir();
    let (code, _, stderr) = run(&dir, &["build", "hello.zz"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(stderr.contains("no clang found"), "needs no-clang error");
    assert!(dir.join("app.c").is_file(), "app.c missing in CWD");
    assert!(dir.join("build.sh").is_file(), "build.sh missing in CWD");
    assert!(!dir.join("bin").exists(), "legacy bin/ must not appear");
    assert!(!dir.join("hello").exists(), "no binary should exist");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_project_no_arg_emits_to_root_bin() {
    let dir = project_dir();
    let (code, _, stderr) = run(&dir, &["build"], &no_clang());
    assert_eq!(
        code, 1,
        "no-clang project build must fail.\nstderr:\n{stderr}"
    );
    assert!(stderr.contains("no clang found"), "needs no-clang error");
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    assert!(dir.join("bin/build.sh").is_file(), "bin/build.sh missing");
    assert!(
        !dir.join("src/bin").exists(),
        "legacy src/bin/ must never be written"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_explicit_entry_emits_to_root_bin() {
    let dir = project_dir();
    let (code, _, stderr) = run(&dir, &["build", "src/main.zz"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    assert!(
        !dir.join("src/bin").exists(),
        "explicit src/main.zz must not create src/bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_non_entry_emits_to_root_bin() {
    let dir = project_dir();
    std::fs::write(dir.join("src/tool.zz"), PROG).expect("tool");
    let (code, _, stderr) = run(&dir, &["build", "src/tool.zz"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    assert!(
        !dir.join("src/bin").exists(),
        "non-entry build must not create src/bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_invoked_from_subdir_stays_rooted() {
    let dir = project_dir();
    let sub = dir.join("src");
    let (code, _, stderr) = run(&sub, &["build"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing at root");
    assert!(
        !dir.join("src/bin").exists(),
        "subdir invocation must not create src/bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_absolute_path_from_other_dir_finds_owning_project() {
    let dir = project_dir();
    let entry = dir.join("src/main.zz");
    let elsewhere = fresh_root("elsewhere");
    let (code, _, stderr) = run(&elsewhere, &["build", entry.to_str().unwrap()], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    // Owning project wins: artifacts at the project root, nothing beside
    // the invocation directory.
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing at root");
    assert!(
        !elsewhere.join("app.c").exists(),
        "invocation dir must stay clean"
    );
    assert!(
        !dir.join("src/bin").exists(),
        "legacy src/bin/ must never be written"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn no_clang_cross_project_explicit_wins_over_cwd() {
    let a = project_dir();
    let b = fresh_root("otherproj");
    std::fs::write(
        b.join("zz.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(b.join("src")).expect("src");
    std::fs::write(b.join("src/main.zz"), PROG).expect("entry");
    // Invoked from project B with project A's entry: A's root wins.
    let entry_a = a.join("src/main.zz");
    let (code, _, stderr) = run(&b, &["build", entry_a.to_str().unwrap()], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(a.join("bin/app.c").is_file(), "bin/app.c missing at A root");
    assert!(!b.join("bin").exists(), "CWD project must stay clean");
    assert!(
        !a.join("src/bin").exists(),
        "legacy src/bin/ must never be written"
    );
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

#[test]
fn project_build_ensures_bin_gitignored() {
    let dir = project_dir();
    std::fs::write(dir.join(".gitignore"), "target/\n").expect("gitignore");
    let (code, _, stderr) = run(&dir, &["build"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    let content = std::fs::read_to_string(dir.join(".gitignore")).expect("read gitignore");
    assert!(content.contains("target/"), "existing entries kept");
    assert!(
        content.lines().any(|l| l.trim() == "bin/"),
        "bin/ must be ensured, got:\n{content}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn standalone_build_leaves_gitignore_alone() {
    let (dir, _) = standalone_dir();
    std::fs::write(dir.join(".gitignore"), "target/\n").expect("gitignore");
    let (code, _, stderr) = run(&dir, &["build", "hello.zz"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    let content = std::fs::read_to_string(dir.join(".gitignore")).expect("read gitignore");
    assert_eq!(content, "target/\n", "standalone must not touch gitignore");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clean_from_subdir_cleans_project_root() {
    let dir = project_dir();
    let (code, _, stderr) = run(&dir, &["build"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    let (code, _, stderr) = run(&dir.join("src"), &["clean"], &[]);
    assert_eq!(code, 0, "clean must pass.\nstderr:\n{stderr}");
    assert!(!dir.join("bin").exists(), "root bin/ must be cleaned");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_bare_output_stays_in_resolved_dir() {
    let dir = project_dir();
    let (code, _, stderr) = run(&dir, &["build", "-o", "server", "src/main.zz"], &no_clang());
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    // Bare `-o` keeps the project convention; the C fallback still lands
    // next to the planned destination.
    assert!(dir.join("bin/app.c").is_file(), "bin/app.c missing");
    assert!(!dir.join("server").exists(), "no binary should exist");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_clang_path_output_is_cwd_exact() {
    let dir = project_dir();
    let (code, _, stderr) = run(
        &dir,
        &["build", "-o", "out/srv", "src/main.zz"],
        &no_clang(),
    );
    assert_eq!(code, 1, "no-clang build must fail.\nstderr:\n{stderr}");
    assert!(dir.join("out/app.c").is_file(), "out/app.c missing");
    assert!(
        !dir.join("bin/app.c").exists(),
        "path -o must not fall back to bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- VM run: no arguments default to the entry, no artifacts ---

#[test]
fn vm_run_without_arg_uses_project_entry() {
    let dir = project_dir();
    let (code, stdout, stderr) = run(&dir, &["run"], &[]);
    assert_eq!(
        code, 0,
        "vm run must pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(stdout, "output_ok\n");
    assert!(
        !dir.join("bin").exists(),
        "VM execution must leave no build artifacts"
    );
    assert!(
        !dir.join("src/bin").exists(),
        "VM execution must leave no legacy artifacts"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Happy paths (need a real toolchain; skip otherwise) ---

fn happy_or_skip(dir: &Path, args: &[&str]) -> Option<(String, String)> {
    let (code, stdout, stderr) = run(dir, args, &[]);
    if code != 0 && stderr.contains("no clang found") {
        eprintln!("SKIP: no clang on PATH");
        let _ = std::fs::remove_dir_all(dir);
        return None;
    }
    assert_eq!(
        code, 0,
        "build must pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    Some((stdout, stderr))
}

#[test]
fn standalone_build_lands_in_cwd() {
    let (dir, _) = standalone_dir();
    let Some(_) = happy_or_skip(&dir, &["build", "hello.zz"]) else {
        return;
    };
    let bin = dir.join("hello");
    assert!(bin.is_file(), "./hello missing");
    assert!(!dir.join("bin").exists(), "standalone must not create bin/");
    let out = Command::new(&bin).output().expect("run binary");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "output_ok\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_build_without_arg_uses_package_name() {
    let dir = project_dir();
    let Some(_) = happy_or_skip(&dir, &["build"]) else {
        return;
    };
    let bin = dir.join("bin").join(PKG);
    assert!(bin.is_file(), "bin/{PKG} missing");
    assert!(
        !dir.join("src/bin").exists(),
        "legacy src/bin/ must never be written"
    );
    let out = Command::new(&bin).output().expect("run binary");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "output_ok\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_explicit_entry_matches_bare_build() {
    let dir = project_dir();
    let Some(_) = happy_or_skip(&dir, &["build", "src/main.zz"]) else {
        return;
    };
    assert!(dir.join("bin").join(PKG).is_file(), "bin/{PKG} missing");
    assert!(
        !dir.join("src/bin").exists(),
        "explicit src/main.zz must not create src/bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_non_entry_builds_to_root_bin_by_stem() {
    let dir = project_dir();
    std::fs::write(dir.join("src/tool.zz"), PROG).expect("tool");
    let Some(_) = happy_or_skip(&dir, &["build", "src/tool.zz"]) else {
        return;
    };
    assert!(dir.join("bin/tool").is_file(), "bin/tool missing");
    assert!(
        !dir.join("src/bin").exists(),
        "non-entry build must not create src/bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_from_subdir_without_arg_stays_rooted() {
    let dir = project_dir();
    let sub = dir.join("src");
    let Some(_) = happy_or_skip(&sub, &["build"]) else {
        // happy_or_skip removed `sub` (== dir/src); remove the rest.
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    assert!(dir.join("bin").join(PKG).is_file(), "bin/{PKG} missing");
    assert!(
        !dir.join("src/bin").exists(),
        "subdir invocation must not create src/bin/"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn absolute_path_from_other_dir_builds_owning_project() {
    let dir = project_dir();
    let entry = dir.join("src/main.zz");
    let elsewhere = fresh_root("elsewhere");
    let Some(_) = happy_or_skip(&elsewhere, &["build", entry.to_str().unwrap()]) else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    assert!(dir.join("bin").join(PKG).is_file(), "bin/{PKG} missing");
    assert!(
        !elsewhere.join(PKG).exists() && !elsewhere.join("main").exists(),
        "invocation dir must stay clean"
    );
    assert!(!dir.join("src/bin").exists(), "legacy src/bin/ written");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn output_overrides_in_project_and_standalone() {
    // Bare `-o` in a project stays in the project bin/.
    let dir = project_dir();
    let Some(_) = happy_or_skip(&dir, &["build", "-o", "server", "src/main.zz"]) else {
        return;
    };
    assert!(dir.join("bin/server").is_file(), "bin/server missing");
    assert!(!dir.join("src/bin").exists(), "legacy src/bin/ written");
    let _ = std::fs::remove_dir_all(&dir);

    // Bare `-o` standalone stays in CWD.
    let (sdir, _) = standalone_dir();
    let Some(_) = happy_or_skip(&sdir, &["build", "-o", "server", "hello.zz"]) else {
        return;
    };
    assert!(sdir.join("server").is_file(), "./server missing");
    assert!(
        !sdir.join("bin").exists(),
        "standalone must not create bin/"
    );

    // Path-like `-o` is CWD-exact.
    let Some(_) = happy_or_skip(&sdir, &["build", "-o", "out/app", "hello.zz"]) else {
        return;
    };
    assert!(sdir.join("out/app").is_file(), "out/app missing");
    let _ = std::fs::remove_dir_all(&sdir);
}
