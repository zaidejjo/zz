//! E2E for the P2.0 profiling floor:
//! - `zz dis --vm` prints VM bytecode (not portable IR) with source sites
//! - `zz dis --vm --stats` prints a per-class mix
//! - `ZZ_VM_STATS=1` prints op counters to stderr, leaving stdout intact

use std::path::{Path, PathBuf};
use std::process::Command;

fn zz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_zz")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Write `src` to a unique `.zz` file and return its path.
fn temp_program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zz_disasm_{}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("main.zz");
    std::fs::write(&path, src).expect("write temp program");
    path
}

fn run(args: &[&str], file: &Path, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(zz_bin());
    cmd.args(args).arg(file).current_dir(workspace_root());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("exec zz");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const LOOP: &str = r#"
func main() {
    s := 0
    i := 0
    while i < 100 {
        s = s + i
        i = i + 1
    }
    println(s)
}
"#;

#[test]
fn dis_vm_prints_fused_ops_with_sites() {
    let file = temp_program("ops", LOOP);
    let (code, stdout, stderr) = run(&["dis", "--vm"], &file, &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("; module "),
        "missing module header:\n{stdout}"
    );
    assert!(
        stdout.contains("fn main"),
        "function chunk missing:\n{stdout}"
    );
    // The loop body must fuse `s = s + i` (P2.1's target shape).
    assert!(
        stdout.contains("SlotBinaryInt") || stdout.contains("SlotAddInt"),
        "no fused arithmetic in:\n{stdout}"
    );
    // Sites are resolved to `line:col` at the end of each op line.
    let has_site = stdout.lines().any(|l| {
        let Some((_, tail)) = l.trim_end().rsplit_once(' ') else {
            return false;
        };
        let mut parts = tail.split(':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), None) => {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && b.chars().all(|c| c.is_ascii_digit())
            }
            _ => false,
        }
    });
    assert!(has_site, "no op carries a source site:\n{stdout}");
}

#[test]
fn dis_vm_stats_prints_class_mix() {
    let file = temp_program("stats", LOOP);
    let (code, stdout, stderr) = run(&["dis", "--vm", "--stats"], &file, &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("fast "), "missing fast share:\n{stdout}");
    assert!(
        stdout.contains(";   top:"),
        "missing op histogram:\n{stdout}"
    );
    assert!(
        !stdout.contains("PushConst(0)"),
        "--stats must not dump individual ops:\n{stdout}"
    );
}

#[test]
fn vm_stats_env_prints_to_stderr_only() {
    let file = temp_program("runstats", LOOP);
    let (code, stdout, stderr) = run(&["run"], &file, &[("ZZ_VM_STATS", "1")]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // Program output stays byte-identical (bench harness parses stdout).
    assert_eq!(stdout.trim(), "4950", "stdout polluted:\n{stdout}");
    assert!(
        stderr.contains("vm stats:"),
        "no counter summary on stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("fast share"),
        "no fast-share line:\n{stderr}"
    );
}

#[test]
fn vm_stats_absent_by_default() {
    let file = temp_program("nostats", LOOP);
    let (code, _stdout, stderr) = run(&["run"], &file, &[]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !stderr.contains("vm stats:"),
        "counters leaked without ZZ_VM_STATS:\n{stderr}"
    );
}
