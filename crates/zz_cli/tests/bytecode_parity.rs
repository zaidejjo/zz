//! Bytecode parity: every batchable strict fixture must behave
//! identically through `zz run` and `zz run --bytecode`.
//!
//! Same engine, same program — so stdout, stderr, and exit codes must
//! match EXACTLY (no normalization). The `--bytecode` path lowers to IR,
//! round-trips through `.zzc` bytes (decode + verify + raise), and
//! executes with no AST-derived structures. A second test covers the
//! `.zzc` FILE loop (`build --emit-ir` → `run --bytecode file.zzc`).
//!
//! Scope reuses [`batch_lists::ELIGIBLE`] (quad/error fixtures stay with
//! the quad; exclusions keep their individual natives).

use std::path::PathBuf;
use std::process::Command;

#[path = "batch_lists.rs"]
mod batch_lists;

use batch_lists::ELIGIBLE;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn zz_bin() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug");
    let exe = format!("zz{}", std::env::consts::EXE_SUFFIX);
    dir.join(&exe)
}

fn run_zz(args: &[&str], cwd: &std::path::Path) -> (i32, String, String) {
    let mut cmd = Command::new(zz_bin());
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn zz");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match child.try_wait().expect("wait") {
            Some(status) => {
                let out = child.wait_with_output().expect("output");
                return (
                    status.code().unwrap_or(124),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                );
            }
            None => {
                if std::time::Instant::now() > deadline {
                    child.kill().ok();
                    return (124, String::new(), "timeout".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

/// Check one fixture: `run` vs `run --bytecode`, byte-identical.
fn check_fixture(cat: &str, file: &str) -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = fixtures_dir().join(cat).join(file);
    let (code, out, err) = run_zz(&["run", &fixture.display().to_string()], &root);
    if code != 0 {
        return Err(format!("{cat}/{file}: zz run exit {code}:\n{err}"));
    }
    let (bcode, bout, berr) = run_zz(
        &["run", "--bytecode", &fixture.display().to_string()],
        &root,
    );
    if bcode != code {
        return Err(format!(
            "{cat}/{file}: exit differs (run={code}, bytecode={bcode}):\n{berr}"
        ));
    }
    if bout != out {
        return Err(format!(
            "{cat}/{file}: stdout differs:\n--- run ---\n{out}\n--- bytecode ---\n{bout}"
        ));
    }
    if berr != err {
        return Err(format!(
            "{cat}/{file}: stderr differs:\n--- run ---\n{err}\n--- bytecode ---\n{berr}"
        ));
    }
    Ok(())
}

#[test]
fn bytecode_parity_all() {
    assert!(!ELIGIBLE.is_empty());
    let n_workers: usize = std::env::var("ZZ_BYTECODE_JOBS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(8)
        .min(ELIGIBLE.len().max(1));
    let slots: Vec<std::sync::Mutex<Option<Result<(), String>>>> = (0..ELIGIBLE.len())
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..n_workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if i >= ELIGIBLE.len() {
                    break;
                }
                let (cat, file, _) = ELIGIBLE[i];
                *slots[i].lock().expect("slot") = Some(check_fixture(cat, file));
            });
        }
    });
    let mut failures = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        match slot.lock().expect("slot").take() {
            Some(Ok(())) => {}
            Some(Err(msg)) => failures.push(msg),
            None => {
                let (cat, file, _) = ELIGIBLE[i];
                failures.push(format!("{cat}/{file}: worker left fixture unrun"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "bytecode parity failures:\n{}",
        failures.join("\n---\n")
    );
}

/// The `.zzc` FILE loop on three single-module fixtures: emit to bytes,
/// disassemble (proves the file decodes), load with no frontend.
#[test]
fn bytecode_file_roundtrip() {
    let cases = [
        ("stdlib", "json_test.zz"),
        ("syntax", "declarations.zz"),
        ("stdlib", "strings.zz"),
    ];
    // All covered by the main parity test (guard against drift).
    for (cat, file) in &cases {
        assert!(
            ELIGIBLE.iter().any(|(c, f, _)| c == cat && f == file),
            "{cat}/{file} left ELIGIBLE"
        );
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sandbox = std::env::temp_dir().join(format!(
        "zz_bytecode_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&sandbox).expect("mkdir sandbox");
    let mut failures = Vec::new();
    for (cat, file) in &cases {
        let fixture = fixtures_dir().join(cat).join(file);
        let zzcz = sandbox.join(format!("{file}.zzc"));
        let zzcz = zzcz.display().to_string();
        let (code, _, err) = run_zz(
            &[
                "build",
                "--emit-ir",
                "-o",
                &zzcz,
                &fixture.display().to_string(),
            ],
            &root,
        );
        if code != 0 {
            failures.push(format!("{cat}/{file}: emit-ir exit {code}:\n{err}"));
            continue;
        }
        let (code, dis_out, dis_err) = run_zz(&["dis", &zzcz], &root);
        if code != 0 || !dis_out.contains("func f0") {
            failures.push(format!("{cat}/{file}: dis failed:\n{dis_err}"));
            continue;
        }
        let (code, out, err) = run_zz(&["run", &fixture.display().to_string()], &root);
        assert_eq!(code, 0);
        let (bcode, bout, berr) = run_zz(&["run", "--bytecode", &zzcz], &root);
        if bcode != code || bout != out || berr != err {
            failures.push(format!("{cat}/{file}: .zzc load differs"));
        }
    }
    if failures.is_empty() {
        let _ = std::fs::remove_dir_all(&sandbox);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}
