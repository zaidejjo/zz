//! End-to-end coverage for `[collect.*]` directory collections (#385):
//! manifest validation, add/remove discovery, diagnostics, and VM/AOT
//! parity on the tracked fixture project.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/collections")
}

fn unique_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "zz_collections_e2e_{tag}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_project(root: &Path, member_sources: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(
        root.join("zz.toml"),
        "[package]\nname = \"collections-e2e\"\nversion = \"0.1.0\"\n\
         [collect.commands]\ndir = \"commands\"\nsig = \"func([str]) -> int\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/main.zz"),
        "import collect.commands\n\
         func main() {\n\
             for entry in commands.all() { println(entry.name) }\n\
         }\n",
    )
    .unwrap();
    for (name, source) in member_sources {
        std::fs::write(root.join("commands").join(name), source).unwrap();
    }
}

fn run_zz(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zz"));
    cmd.args(args)
        .env("ZZ_RUN_CACHE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(workspace_root());
    let output = cmd.output().expect("run zz");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

const MAIN: &str = "src/main.zz";

#[test]
fn fixture_checks_and_runs_in_vm() {
    let root = fixture_root();
    let (code, stdout, stderr) = run_zz(&["run", root.join(MAIN).to_str().unwrap()]);
    assert_eq!(code, 0, "stderr:\n{stderr}");
    assert_eq!(
        stdout,
        "collection_ok\nmember build\nmember greet\nget greet\nnone\n"
    );

    let (code, _stdout, stderr) = run_zz(&["check", root.join(MAIN).to_str().unwrap()]);
    assert_eq!(code, 0, "check stderr:\n{stderr}");
}

#[test]
fn fixture_runs_identically_in_aot() {
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return;
    }
    let root = fixture_root();
    let out_dir = unique_dir("native");
    let out = out_dir.join("collections_demo");

    let (vm_code, vm_stdout, vm_stderr) = run_zz(&["run", root.join(MAIN).to_str().unwrap()]);
    assert_eq!(vm_code, 0, "vm stderr:\n{vm_stderr}");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zz"));
    cmd.args([
        "run",
        "--native",
        root.join(MAIN).to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ])
    .env("ZZ_RUN_CACHE", "0")
    .env("ZZ_NATIVE_DEV", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .current_dir(workspace_root());
    let output = cmd.output().expect("run zz --native");
    let native_code = output.status.code().unwrap_or(-1);
    let native_stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let native_stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let _ = std::fs::remove_dir_all(&out_dir);

    assert_eq!(
        native_code, vm_code,
        "native stderr:\n{native_stderr}\nvm stderr:\n{vm_stderr}"
    );
    assert_eq!(native_stdout, vm_stdout, "VM/AOT output diverged");
}

fn run_zz_cached(args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zz"));
    cmd.args(args)
        .env_remove("ZZ_RUN_CACHE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(workspace_root());
    let output = cmd.output().expect("run zz");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn changing_sig_invalidates_the_run_cache() {
    let root = unique_dir("cache_run");
    write_project(
        &root,
        &[(
            "greet.zz",
            "pub func run(_args: [str]) -> int { return 0 }\n",
        )],
    );
    let main = root.join(MAIN);
    let main = main.to_str().unwrap();

    let (code, stdout, stderr) = run_zz_cached(&["run", main]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "greet\n");

    std::fs::write(
        root.join("zz.toml"),
        "[package]\nname = \"collections-e2e\"\nversion = \"0.1.0\"\n\
         [collect.commands]\ndir = \"commands\"\nsig = \"func([str]) -> str\"\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_zz_cached(&["run", main]);
    let _ = std::fs::remove_dir_all(&root);
    assert_ne!(
        code, 0,
        "stale run cache served the old collection: {stdout}"
    );
    assert!(
        stderr.contains("does not match collection `commands`"),
        "{stderr}"
    );
}

#[test]
fn changing_sig_invalidates_the_check_cache() {
    let root = unique_dir("cache_sig");
    write_project(
        &root,
        &[(
            "greet.zz",
            "pub func run(_args: [str]) -> int { return 0 }\n",
        )],
    );
    let main = root.join(MAIN);
    let main = main.to_str().unwrap();

    // Warm the S1 check cache for the synthesized module.
    let (code, _stdout, stderr) = run_zz(&["check", main]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _stdout, stderr) = run_zz(&["check", main]);
    assert_eq!(code, 0, "{stderr}");

    std::fs::write(
        root.join("zz.toml"),
        "[package]\nname = \"collections-e2e\"\nversion = \"0.1.0\"\n\
         [collect.commands]\ndir = \"commands\"\nsig = \"func([str]) -> str\"\n",
    )
    .unwrap();
    let (code, _stdout, stderr) = run_zz(&["check", main]);
    let _ = std::fs::remove_dir_all(&root);
    assert_ne!(code, 0, "stale cache accepted the old signature");
    assert!(
        stderr.contains("does not match collection `commands`"),
        "{stderr}"
    );
}

#[test]
fn wrong_signature_fails_check_with_expected_and_found() {
    let root = unique_dir("mismatch");
    write_project(
        &root,
        &[
            ("ok.zz", "pub func run(_args: [str]) -> int { return 0 }\n"),
            (
                "bad.zz",
                "pub func run(_args: [str]) -> str { return \"nope\" }\n",
            ),
        ],
    );
    let (code, _stdout, stderr) = run_zz(&["check", root.join(MAIN).to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(&root);
    assert_ne!(code, 0, "wrong signature must fail check");
    assert!(
        stderr.contains("does not match collection `commands`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("expected: func run(args: [str]) -> int"),
        "{stderr}"
    );
    assert!(
        stderr.contains("found:    func run(_args: [str]) -> str"),
        "{stderr}"
    );
}

#[test]
fn adding_and_removing_files_updates_the_collection() {
    let root = unique_dir("discovery");
    write_project(
        &root,
        &[(
            "greet.zz",
            "pub func run(_args: [str]) -> int { return 0 }\n",
        )],
    );
    let main = root.join(MAIN);
    let main = main.to_str().unwrap();

    let (code, stdout, stderr) = run_zz(&["run", main]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "greet\n");

    std::fs::write(
        root.join("commands/build.zz"),
        "pub func run(_args: [str]) -> int { return 0 }\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run_zz(&["run", main]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "build\ngreet\n");

    std::fs::remove_file(root.join("commands/greet.zz")).unwrap();
    let (code, stdout, stderr) = run_zz(&["run", main]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "build\n");
    let _ = std::fs::remove_dir_all(&root);
}
