//! Regression tests for zz-lsp CLI flags (#255): `--version`/`--help`
//! must short-circuit instead of feeding the LSP parser (which answered
//! with a JSON-RPC parse error, hiding the server build from editors).

use std::process::Command;

fn lsp_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_zz-lsp"))
}

#[test]
fn version_prints_without_lsp_framing() {
    let out = Command::new(lsp_bin())
        .arg("--version")
        .output()
        .expect("spawn zz-lsp --version");
    assert!(out.status.success(), "exit: {}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("zz-lsp "),
        "expected `zz-lsp <version>`, got: {stdout:?}"
    );
    assert!(
        !stdout.contains("jsonrpc"),
        "flag leaked into LSP parser: {stdout:?}"
    );
}

#[test]
fn help_prints_usage() {
    let out = Command::new(lsp_bin())
        .arg("--help")
        .output()
        .expect("spawn zz-lsp --help");
    assert!(out.status.success(), "exit: {}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("usage: zz-lsp"), "got: {stdout:?}");
}
