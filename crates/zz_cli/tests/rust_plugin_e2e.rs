//! Hermetic Rust plugin E2E (#236): the checked-in `examples/rust-fnv`
//! package has NO build script and NO `build.rs`. `zz install` must vendor
//! (empty closure), drive `cargo build --offline --locked` itself, and the
//! resulting plugin must pass both engines plus `zz audit`.
//!
//! The test is runtime-gated on cargo/rustc (like the `cc` E2E is gated on
//! a C toolchain): without a Rust toolchain the backend correctly refuses,
//! so there is nothing to assert end-to-end.

// `zz run` cannot dlopen plugins on non-Unix hosts (`load_vm_plugins` is a
// no-op stub there), so every assertion below is Unix-only by construction.
// The declarative AOT half is covered on Windows by `zz_pm`'s build tests.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn zz_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    deps.parent().unwrap().join("zz")
}

fn run_zz(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("zz binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            if entry.file_name() == "target" || entry.file_name() == "build" {
                continue;
            }
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn toolchain_available() -> bool {
    ["cargo", "rustc"].iter().all(|bin| {
        Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

const EXPECTED: &str = "-6615550055289275125\n-3750763034362895579\n-20207\nfnv_ok\n";

#[test]
fn rust_plugin_builds_without_scripts_and_engines_agree() {
    if !toolchain_available() {
        eprintln!("skipping: cargo/rustc unavailable");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = root.join("examples/rust-fnv");
    assert!(source.is_dir(), "example missing at {}", source.display());

    let dir = std::env::temp_dir().join(format!("zz-rust-fnv-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(&source, &dir);
    let pkg = dir.join("fnv");
    let consumer = dir.join("consumer");

    // Contract: no package-supplied build code ships with the example.
    assert!(
        !pkg.join("build.sh").exists(),
        "example must not use a hook"
    );
    assert!(
        !pkg.join("native/build.rs").exists(),
        "example must not use build.rs"
    );

    let (code, _, stderr) = run_zz(&consumer, &["install"]);
    assert_eq!(code, 0, "install failed: {stderr}");

    // AOT: static link from the Rust staticlib.
    let (code, _, stderr) = run_zz(&consumer, &["build", "src/main.zz"]);
    assert_eq!(code, 0, "build failed: {stderr}");
    let bin = consumer.join("bin/fnvcheck");
    assert!(bin.is_file(), "project binary missing");
    let out = Command::new(&bin).output().expect("binary should run");
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);

    // VM: dlopen the Rust cdylib through the C-ABI path.
    let (code, stdout, stderr) = run_zz(&consumer, &["run", "src/main.zz"]);
    assert_eq!(code, 0, "run failed: {stderr}");
    assert_eq!(stdout, EXPECTED);

    // Audit surfaces backend/tag/rustc, the closure key, and licenses.
    let (code, stdout, stderr) = run_zz(&consumer, &["audit"]);
    assert_eq!(code, 0, "audit failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("backend=rust"), "{stdout}");
    assert!(stdout.contains("vendor="), "{stdout}");

    // Tampering with consumed Rust sources must fail the audit (the
    // closure key covers Cargo.toml + Cargo.lock + crate sources).
    std::fs::write(pkg.join("native/src/lib.rs"), "// tampered after install\n").unwrap();
    let (code, stdout, stderr) = run_zz(&consumer, &["audit"]);
    assert_ne!(
        code, 0,
        "tampered Rust source passed audit:\n{stdout}\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
