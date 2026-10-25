//! `#586` — `--help` must never mutate the workspace.
//!
//! `zz fix --help` used to run the fixer over the whole project and
//! rewrite every file; `zz fmt --help` reformatted them; `zz check --fix
//! --help` applied fixes. A help request is read-only by definition, so
//! every writing command must answer `--help`/`-h` with usage text and
//! exit 0 before touching the filesystem.
//!
//! The second half of the issue is the misleading report position: the
//! fix report printed a raw byte offset as if it were a line number
//! (`path:56` for an 8-line file). It must print a real `line:column`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A file the checker can fix: the function returns `int` but its body
/// falls through, so `zz fix` inserts `return `.
const FIXABLE: &str = "func alpha() -> int {\n    1\n}\n\nfunc beta() -> int {\n    2\n}\n";

/// An unformatted file: `zz fmt` would rewrite it.
const UNFORMATTED: &str = "func alpha() -> int {\n  1\n}\n";

fn zz() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zz-help-nomutate-{tag}-{}-{uniq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmpdir");
    dir
}

fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(zz())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("exec zz");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every writing command answers `--help` with usage, exits 0, and leaves
/// the file byte-identical.
fn assert_help_is_read_only(tag: &str, args: &[&str], content: &str) {
    let dir = fresh_dir(tag);
    let file = dir.join("two.zz");
    std::fs::write(&file, content).expect("fixture");
    let before = std::fs::read(&file).expect("read");

    let (code, stdout, stderr) = run(&dir, args);
    let after = std::fs::read(&file).expect("read");

    assert_eq!(
        code,
        0,
        "`zz {}` should exit 0, stderr: {stderr}",
        args.join(" ")
    );
    assert!(
        stdout.contains("usage:"),
        "`zz {}` should print usage on stdout, got: {stdout}",
        args.join(" ")
    );
    assert_eq!(
        before,
        after,
        "`zz {}` must not modify the file",
        args.join(" ")
    );
}

#[test]
fn fix_help_does_not_apply_fixes() {
    assert_help_is_read_only("fix-help", &["fix", "--help"], FIXABLE);
}

#[test]
fn fix_short_help_does_not_apply_fixes() {
    assert_help_is_read_only("fix-h", &["fix", "-h"], FIXABLE);
}

#[test]
fn check_help_does_not_run_the_checker() {
    assert_help_is_read_only("check-help", &["check", "--help"], FIXABLE);
}

#[test]
fn check_fix_help_does_not_apply_fixes() {
    assert_help_is_read_only("check-fix-help", &["check", "--fix", "--help"], FIXABLE);
}

#[test]
fn fmt_help_does_not_reformat() {
    assert_help_is_read_only("fmt-help", &["fmt", "--help"], UNFORMATTED);
}

#[test]
fn fmt_short_help_does_not_reformat() {
    assert_help_is_read_only("fmt-h", &["fmt", "-h"], UNFORMATTED);
}

#[test]
fn fix_report_prints_line_and_column_not_a_byte_offset() {
    let dir = fresh_dir("fix-report");
    let file = dir.join("two.zz");
    std::fs::write(&file, FIXABLE).expect("fixture");

    let (code, _stdout, stderr) = run(&dir, &["fix", "two.zz"]);
    assert_eq!(code, 0, "fix should succeed: {stderr}");

    // The inserted `return ` belongs to the body of `alpha` — line 2 —
    // and to the body of `beta` — line 6. The old report printed the
    // byte offset (`26`), which read like a line number in a file that
    // only has 8 lines.
    assert!(
        stderr.contains("two.zz:2:5"),
        "expected a `line:column` position, got: {stderr}"
    );
    assert!(
        stderr.contains("two.zz:6:5"),
        "expected a `line:column` position, got: {stderr}"
    );
    assert!(
        !stderr.contains("two.zz:26"),
        "byte offset must not be printed as a line number: {stderr}"
    );
}
