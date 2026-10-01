//! `[package] zz` minimum-compiler enforcement (run/build/test/add/install).
//!
//! Each test builds a scratch project under the temp dir (never the repo)
//! and drives the freshly built `zz` binary at it. Path deps keep everything
//! offline; nothing touches the registry.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn zz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_zz")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zz_req_test_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    d
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn run_zz(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn zz");
    let code = out.status.code().unwrap_or(-1);
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn main_zz() -> &'static str {
    "println(\"hello\")\nprintln(\"REQ_OK\")\n"
}

#[test]
fn run_rejects_unsatisfiable_req() {
    let d = scratch("run_reject");
    write(
        &d.join("zz.toml"),
        "[package]\nname = \"needy\"\nversion = \"0.1.0\"\nzz = \">=999.0.0\"\n",
    );
    write(&d.join("src").join("main.zz"), main_zz());
    let (code, _, err) = run_zz(&d, &["run", "src/main.zz"]);
    assert_ne!(code, 0, "must fail on unsatisfied zz req");
    assert!(
        err.contains("999") && err.contains("upgrade"),
        "needs req + upgrade hint, got:\n{err}"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn run_accepts_satisfied_and_absent_req() {
    let d = scratch("run_ok");
    write(
        &d.join("zz.toml"),
        "[package]\nname = \"fine\"\nversion = \"0.1.0\"\nzz = \">=0.1.0\"\n",
    );
    write(&d.join("src").join("main.zz"), main_zz());
    let (code, out, _) = run_zz(&d, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "satisfied req must run");
    assert!(out.contains("REQ_OK"), "unexpected output:\n{out}");

    let d2 = scratch("run_absent");
    write(
        &d2.join("zz.toml"),
        "[package]\nname = \"legacy\"\nversion = \"0.1.0\"\n",
    );
    write(&d2.join("src").join("main.zz"), main_zz());
    let (code, out, _) = run_zz(&d2, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "absent req must run");
    assert!(out.contains("REQ_OK"), "unexpected output:\n{out}");
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&d2);
}

#[test]
fn build_rejects_unsatisfiable_req() {
    let d = scratch("build_reject");
    write(
        &d.join("zz.toml"),
        "[package]\nname = \"needy\"\nversion = \"0.1.0\"\nzz = \">=999.0.0\"\n",
    );
    write(&d.join("src").join("main.zz"), main_zz());
    let (code, _, err) = run_zz(&d, &["build", "src/main.zz"]);
    assert_ne!(code, 0, "build must fail on unsatisfied zz req");
    assert!(err.contains("upgrade"), "needs upgrade hint, got:\n{err}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn test_rejects_unsatisfiable_req() {
    let d = scratch("test_reject");
    write(
        &d.join("zz.toml"),
        "[package]\nname = \"needy\"\nversion = \"0.1.0\"\nzz = \">=999.0.0\"\n",
    );
    write(
        &d.join("src").join("main.zz"),
        "@test\nfunc t_req() {\n    assert(true)\n}\n",
    );
    let (code, _, err) = run_zz(&d, &["test", "src/"]);
    assert_ne!(code, 0, "test must fail on unsatisfied zz req");
    assert!(err.contains("upgrade"), "needs upgrade hint, got:\n{err}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn install_errors_on_dep_req() {
    let d = scratch("install_dep");
    fs::create_dir_all(d.join("dep").join("src")).unwrap();
    write(
        &d.join("dep").join("zz.toml"),
        "[package]\nname = \"future-dep\"\nversion = \"0.1.0\"\nzz = \">=999.0.0\"\n",
    );
    write(&d.join("dep").join("src").join("main.zz"), main_zz());
    write(
        &d.join("zz.toml"),
        "[package]\nname = \"root\"\nversion = \"0.1.0\"\n\n[dependencies]\nfuture-dep = { path = \"dep\" }\n",
    );
    write(&d.join("src").join("main.zz"), main_zz());
    let (code, _, err) = run_zz(&d, &["install"]);
    assert_ne!(code, 0, "install must fail on dep zz req");
    assert!(
        err.contains("future-dep") && err.contains("upgrade"),
        "needs dep name + upgrade hint, got:\n{err}"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn install_path_errors_on_tool_req() {
    // `zz install --path <dir>` builds a tool: an unsatisfiable `zz`
    // requirement must fail before clang runs.
    let d = scratch("install_path");
    fs::create_dir_all(d.join("tool").join("src")).unwrap();
    write(
        &d.join("tool").join("zz.toml"),
        "[package]\nname = \"future-tool\"\nversion = \"0.1.0\"\nzz = \">=999.0.0\"\n",
    );
    write(&d.join("tool").join("src").join("main.zz"), main_zz());
    let (code, _, err) = run_zz(&d, &["install", "--path", "tool"]);
    assert_ne!(code, 0, "tool install must fail on zz req");
    assert!(
        err.contains("future-tool") && err.contains("upgrade"),
        "needs tool name + upgrade hint, got:\n{err}"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn add_warns_on_path_dep_req() {
    // `zz add --path` on a tool-style dep warns but still records (and
    // installs); the library `zz install` leg is what hard-errors.
    let d = scratch("add_warn");
    fs::create_dir_all(d.join("dep").join("src")).unwrap();
    write(
        &d.join("dep").join("zz.toml"),
        "[package]\nname = \"future-dep\"\nversion = \"0.1.0\"\nzz = \">=999.0.0\"\n",
    );
    write(&d.join("dep").join("src").join("main.zz"), main_zz());
    write(
        &d.join("zz.toml"),
        "[package]\nname = \"root\"\nversion = \"0.1.0\"\n",
    );
    write(&d.join("src").join("main.zz"), main_zz());
    // `zz add --path` on a tool-style dep warns, records, then its
    // install step blocks on the requirement with the upgrade hint.
    let (code, _, err) = run_zz(&d, &["add", "--path", "dep"]);
    assert_ne!(code, 0, "tool install must block on dep zz req");
    assert!(
        err.contains("warning") && err.contains("future-dep") && err.contains("999"),
        "needs the add-time warning, got:\n{err}"
    );
    assert!(
        err.contains("upgrade"),
        "needs the install-time upgrade hint, got:\n{err}"
    );
    let manifest = fs::read_to_string(d.join("zz.toml")).unwrap();
    assert!(
        manifest.contains("future-dep") || manifest.contains("path = \"dep\""),
        "dep must still be recorded:\n{manifest}"
    );
    let _ = fs::remove_dir_all(&d);
}
