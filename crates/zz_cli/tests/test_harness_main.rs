//! Regression test for #401: `zz test --engine=native` on a package whose
//! test file imports a module named `main`.
//!
//! The AOT harness inlines the test file and appends its own `func main`
//! entrypoint, so a bare `import <pkg>.main` collided: value scope
//! shadowed the module and `main.answer()` failed to type-check
//! ("cannot access field `answer` ..."), while the VM (no injected
//! entrypoint) stayed green. The harness now retargets such imports
//! through a file-local alias.
//!
//! Scaffolds a temp package (`zz.toml` + `src/main.zz` + `tests/t.zz`),
//! runs `zz test` on the VM and, unless `ZZ_SKIP_NATIVE=1`, on AOT too.
//! Skips the native leg when the backend is unavailable, mirroring pg_aot.

use std::path::PathBuf;
use std::process::Command;

fn zz_bin() -> PathBuf {
    // `CARGO_BIN_EXE_zz` is the built binary for this test run and
    // carries the platform executable suffix (`zz.exe` on Windows).
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn scaffold() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zz_harness_main_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::create_dir_all(dir.join("tests")).expect("mkdir tests");
    std::fs::write(
        dir.join("zz.toml"),
        "[package]\nname = \"nrepro\"\nversion = \"0.1.0\"\n",
    )
    .expect("write zz.toml");
    std::fs::write(
        dir.join("src").join("main.zz"),
        "pub func answer() -> int { return 42 }\n",
    )
    .expect("write src/main.zz");
    std::fs::write(
        dir.join("tests").join("t.zz"),
        "import nrepro.main\n@test\nfunc calls_main_module() {\n    assert_eq(main.answer(), 42)\n}\n",
    )
    .expect("write tests/t.zz");
    dir
}

fn run_zz(dir: &PathBuf, extra: &[&str]) -> (bool, String) {
    let out = Command::new(zz_bin())
        .arg("test")
        .args(extra)
        .current_dir(dir)
        .output()
        .expect("run zz test");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

#[test]
fn harness_main_module_import_passes_both_engines() {
    let dir = scaffold();
    // VM leg always runs.
    let (ok, text) = run_zz(&dir, &[]);
    assert!(ok, "VM `zz test` failed:\n{text}");
    assert!(
        text.contains("1 passed"),
        "VM output missing pass marker:\n{text}"
    );
    // Native leg is the #401 regression (skipped without a backend).
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native leg (ZZ_SKIP_NATIVE=1)");
        std::fs::remove_dir_all(&dir).ok();
        return;
    }
    let (ok, text) = run_zz(&dir, &["--engine=native"]);
    assert!(ok, "native `zz test` failed:\n{text}");
    assert!(
        text.contains("1 passed"),
        "native output missing pass marker:\n{text}"
    );
    assert!(
        !text.contains("cannot access field"),
        "collision error resurfaced:\n{text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
