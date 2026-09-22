//! CLI end-to-end integration tests for `zz fmt`.
//!
//! Exercises the `zz fmt` binary directly for:
//! - In-place formatting
//! - `--check` mode (exits 1 if unformatted, prints diff)
//! - `--stdin` mode (reads from stdin, writes formatted to stdout)

use std::path::PathBuf;
use std::process::Command;

/// Locate the `zz` binary built by cargo.
fn zz_bin() -> PathBuf {
    // CARGO_BIN_EXE_zz is only available when compiling the crate that defines
    // the binary. Since we're in zz_fmt, locate it from the workspace root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.join("../..");
    // Try debug first, then release.
    let debug = workspace.join("target/debug/zz");
    if debug.exists() {
        return debug;
    }
    let release = workspace.join("target/release/zz");
    if release.exists() {
        return release;
    }
    panic!(
        "zz binary not found. Run `cargo build` first.\n  looked in: {} and {}",
        debug.display(),
        release.display()
    );
}

/// Run `zz fmt [args...]` with optional stdin, return (exit_code, stdout, stderr).
fn run_zz_fmt(args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(zz_bin());
    cmd.args(args);
    if let Some(input) = stdin {
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn zz fmt");
        if let Some(mut stdin_pipe) = child.stdin.take() {
            use std::io::Write;
            stdin_pipe.write_all(input.as_bytes()).expect("write stdin");
        }
        let output = child.wait_with_output().expect("failed to wait on zz fmt");
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (exit_code, stdout, stderr)
    } else {
        let output = cmd.output().expect("failed to exec zz fmt");
        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (exit_code, stdout, stderr)
    }
}

/// Create a temp directory with a `.zz` file and return the path.
fn setup_temp_file(name: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zz_fmt_cli_e2e_{}_{}",
        name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.zz"));
    std::fs::write(&path, content).unwrap();
    path
}

// ── In-place formatting tests ─────────────────────────────────────────────

#[test]
fn fmt_in_place_writes_formatted_file() {
    let unformatted = "x:=1\ny := 2\n";
    let path = setup_temp_file("in_place", unformatted);
    let path_str = path.display().to_string();

    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", &path_str], None);
    assert_eq!(exit, 0, "zz fmt should succeed.\nstderr: {stderr}");

    let result = std::fs::read_to_string(&path).expect("read formatted file");
    assert!(
        result.contains("x := 1"),
        "should format assignment: {result}"
    );
    assert!(result.contains("y := 2"), "should preserve y: {result}");

    // Cleanup.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

#[test]
fn fmt_in_place_no_change() {
    let already_formatted = "x := 1\ny := 2\n";
    let path = setup_temp_file("no_change", already_formatted);
    let path_str = path.display().to_string();

    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", &path_str], None);
    assert_eq!(exit, 0, "zz fmt should succeed.\nstderr: {stderr}");
    assert!(
        stderr.contains("all files already formatted"),
        "should report no changes needed: {stderr}"
    );

    let result = std::fs::read_to_string(&path).expect("read file");
    assert_eq!(result, already_formatted, "file should be unchanged");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

// ── --check mode tests ────────────────────────────────────────────────────

#[test]
fn fmt_check_unformatted_exits_1() {
    let unformatted = "x:=1\ny := 2\n";
    let path = setup_temp_file("check_unfmt", unformatted);
    let path_str = path.display().to_string();

    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", "--check", &path_str], None);
    assert_ne!(
        exit, 0,
        "zz fmt --check should exit 1 for unformatted code.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("would reformat"),
        "should indicate file needs formatting: {stderr}"
    );

    // File should NOT be modified.
    let result = std::fs::read_to_string(&path).expect("read file");
    assert_eq!(result, unformatted, "file should remain unformatted");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

#[test]
fn fmt_check_formatted_exits_0() {
    let formatted = "x := 1\ny := 2\n";
    let path = setup_temp_file("check_fmt", formatted);
    let path_str = path.display().to_string();

    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", "--check", &path_str], None);
    assert_eq!(
        exit, 0,
        "zz fmt --check should exit 0 for formatted code.\nstderr: {stderr}"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

#[test]
fn fmt_check_short_alias_c() {
    let unformatted = "a:=1\n";
    let path = setup_temp_file("check_c", unformatted);
    let path_str = path.display().to_string();

    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", "-c", &path_str], None);
    assert_ne!(
        exit, 0,
        "zz fmt -c should exit 1 for unformatted code.\nstderr: {stderr}"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(path.parent().unwrap());
}

// ── --stdin mode tests ────────────────────────────────────────────────────

#[test]
fn stdin_outputs_formatted() {
    let input = "x:=1\ny := 2\n";
    let (exit, stdout, stderr) = run_zz_fmt(&["fmt", "--stdin"], Some(input));
    assert_eq!(exit, 0, "zz fmt --stdin should succeed.\nstderr: {stderr}");
    assert!(
        stdout.contains("x := 1"),
        "stdout should be formatted: {stdout}"
    );
    assert!(
        stdout.contains("y := 2"),
        "stdout should preserve y: {stdout}"
    );
}

#[test]
fn stdin_no_change_outputs_same() {
    let input = "x := 1\ny := 2\n";
    let (exit, stdout, _stderr) = run_zz_fmt(&["fmt", "--stdin"], Some(input));
    assert_eq!(exit, 0);
    assert_eq!(stdout, input, "already formatted stdin should pass through");
}

#[test]
fn stdin_check_unformatted_exits_1() {
    let input = "x:=1\n";
    let (exit, _stdout, _stderr) = run_zz_fmt(&["fmt", "--check", "--stdin"], Some(input));
    assert_ne!(
        exit, 0,
        "zz fmt --check --stdin should exit 1 for unformatted input"
    );
}

#[test]
fn stdin_check_formatted_exits_0() {
    let input = "x := 1\n";
    let (exit, _stdout, _stderr) = run_zz_fmt(&["fmt", "--check", "--stdin"], Some(input));
    assert_eq!(
        exit, 0,
        "zz fmt --check --stdin should exit 0 for formatted input"
    );
}

// ── Directory formatting test ─────────────────────────────────────────────

#[test]
fn fmt_directory_formats_all_files() {
    let dir = std::env::temp_dir().join(format!(
        "zz_fmt_cli_e2e_dir_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let file_a = dir.join("a.zz");
    let file_b = dir.join("b.zz");
    std::fs::write(&file_a, "x:=1\n").unwrap();
    std::fs::write(&file_b, "y:=2\n").unwrap();

    let dir_str = dir.display().to_string();
    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", &dir_str], None);
    assert_eq!(
        exit, 0,
        "zz fmt on directory should succeed.\nstderr: {stderr}"
    );

    let result_a = std::fs::read_to_string(&file_a).unwrap();
    let result_b = std::fs::read_to_string(&file_b).unwrap();
    assert!(
        result_a.contains("x := 1"),
        "file a should be formatted: {result_a}"
    );
    assert!(
        result_b.contains("y := 2"),
        "file b should be formatted: {result_b}"
    );

    let _ = std::fs::remove_file(&file_a);
    let _ = std::fs::remove_file(&file_b);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn fmt_directory_check_reports_multiple() {
    let dir = std::env::temp_dir().join(format!(
        "zz_fmt_cli_e2e_dirchk_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let file_a = dir.join("a.zz");
    let file_b = dir.join("b.zz");
    std::fs::write(&file_a, "x:=1\n").unwrap();
    std::fs::write(&file_b, "y:=2\n").unwrap();

    let dir_str = dir.display().to_string();
    let (exit, _stdout, stderr) = run_zz_fmt(&["fmt", "--check", &dir_str], None);
    assert_ne!(
        exit, 0,
        "zz fmt --check on unformatted dir should exit 1.\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("would reformat"),
        "should report files needing formatting: {stderr}"
    );

    // Files should NOT be modified.
    let result_a = std::fs::read_to_string(&file_a).unwrap();
    let result_b = std::fs::read_to_string(&file_b).unwrap();
    assert_eq!(result_a, "x:=1\n", "file a should remain unformatted");
    assert_eq!(result_b, "y:=2\n", "file b should remain unformatted");

    let _ = std::fs::remove_file(&file_a);
    let _ = std::fs::remove_file(&file_b);
    let _ = std::fs::remove_dir(&dir);
}
