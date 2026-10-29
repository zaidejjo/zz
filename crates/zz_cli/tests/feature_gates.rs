//! Feature-gated modules and optional dependencies (#488), end to end.
//!
//! Each test builds a scratch project under the temp dir (never the repo)
//! and drives the freshly built `zz` binary at it. Path deps keep the
//! install test offline; nothing touches the registry.
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn zz_bin() -> &'static str {
    env!("CARGO_BIN_EXE_zz")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "zz_feature_gate_{name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
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
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const GATED_MANIFEST: &str = "[package]\n\
    name = \"app\"\n\
    version = \"0.1.0\"\n\
    \n\
    [features]\n\
    locales-ar = { modules = [\"locales/ar\"] }\n";

fn gated_project(name: &str) -> PathBuf {
    let d = scratch(name);
    write(&d.join("zz.toml"), GATED_MANIFEST);
    write(
        &d.join("src").join("main.zz"),
        "import locales.ar\n\
         println(ar.greeting())\n",
    );
    write(
        &d.join("src").join("locales").join("ar.zz"),
        "pub func greeting() -> str { return \"FEATURE_AR_OK\" }\n",
    );
    d
}

#[test]
fn run_reports_the_exact_feature_and_enabling_flag() {
    let d = gated_project("run_helpful");
    let (code, _, err) = run_zz(&d, &["run", "src/main.zz"]);
    assert_ne!(code, 0, "disabled feature must fail the run");
    assert!(
        err.contains("module `locales/ar` is provided by feature `locales-ar`"),
        "needs exact module + feature, got:\n{err}"
    );
    assert!(
        err.contains("--features locales-ar"),
        "needs the enabling flag hint, got:\n{err}"
    );
    assert!(
        !err.contains("file not found"),
        "must not degrade to a file-not-found error, got:\n{err}"
    );
}

#[test]
fn run_enables_features_and_keeps_cache_entries_separate() {
    let d = gated_project("run_enable");
    let (code, _, err) = run_zz(&d, &["run", "--features", "locales-ar", "src/main.zz"]);
    assert_eq!(code, 0, "feature-enabled run failed:\n{err}");
    assert!(
        out_has_stdout(
            &d,
            &["run", "--features=locales-ar", "src/main.zz"],
            "FEATURE_AR_OK"
        ),
        "feature-enabled run must execute the gated module"
    );
    // The gated run's cache entry must not leak into a flagless run.
    let (code, _, err) = run_zz(&d, &["run", "src/main.zz"]);
    assert_ne!(
        code, 0,
        "flagless run must not reuse a gated cache entry:\n{err}"
    );
    // `--all-features` is the other spelling and must work too.
    let (code, _, err) = run_zz(&d, &["run", "--all-features", "src/main.zz"]);
    assert_eq!(code, 0, "--all-features run failed:\n{err}");
}

fn out_has_stdout(dir: &Path, args: &[&str], needle: &str) -> bool {
    let (code, out, _) = run_zz(dir, args);
    code == 0 && out.contains(needle)
}

#[test]
fn check_honors_features() {
    let d = gated_project("check");
    let (code, _, err) = run_zz(&d, &["check", "src"]);
    assert_ne!(code, 0, "flagless check must fail on the gated import");
    assert!(
        err.contains("provided by feature `locales-ar`"),
        "check must report the feature gate, got:\n{err}"
    );
    let (code, _, err) = run_zz(&d, &["check", "--features", "locales-ar", "src"]);
    assert_eq!(code, 0, "feature-enabled check failed:\n{err}");
    let (code, _, err) = run_zz(&d, &["check", "--all-features", "src"]);
    assert_eq!(code, 0, "all-features check failed:\n{err}");
}

#[test]
fn unknown_feature_flag_is_loud() {
    let d = gated_project("unknown");
    let (code, _, err) = run_zz(&d, &["run", "--features", "nope", "src/main.zz"]);
    assert_ne!(code, 0, "unknown feature must fail");
    assert!(
        err.contains("unknown feature `nope`") && err.contains("locales-ar"),
        "needs unknown-feature + available list, got:\n{err}"
    );
}

#[test]
fn build_honors_features_and_does_not_reuse_a_gated_cache_entry() {
    if support::native_unsupported() {
        eprintln!("skip: native backend unsupported on this host");
        return;
    }
    if std::process::Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping: clang not available");
        return;
    }
    let d = gated_project("build");
    let (code, out, err) = run_zz(&d, &["build", "--features", "locales-ar", "src/main.zz"]);
    assert_eq!(code, 0, "feature-enabled build failed:\n{err}\n{out}");
    let bin = support::exe_in(d.join("bin"), "app");
    assert!(bin.is_file(), "expected binary at {}", bin.display());
    let run = Command::new(&bin).output().expect("run built binary");
    assert_eq!(run.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("FEATURE_AR_OK"),
        "binary must execute the enabled module"
    );

    // Flagless build must not reuse the gated binary from the build cache.
    let (code, _, err) = run_zz(&d, &["build", "src/main.zz"]);
    assert_ne!(code, 0, "flagless build must fail despite the gated cache");
    assert!(
        err.contains("provided by feature `locales-ar`"),
        "build must report the feature gate, got:\n{err}"
    );
}

#[test]
fn install_skips_disabled_optional_path_dep_and_links_when_enabled() {
    let d = scratch("install");
    write(
        &d.join("zz.toml"),
        "[package]\n\
         name = \"app\"\n\
         version = \"0.1.0\"\n\
         \n\
         [dependencies]\n\
         helper = { path = \"helper\", optional = true }\n\
         \n\
         [features]\n\
         helpers = { deps = [\"helper\"] }\n",
    );
    write(&d.join("src").join("main.zz"), "println(\"app\")\n");
    write(
        &d.join("helper").join("zz.toml"),
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\n",
    );
    write(
        &d.join("helper").join("src").join("main.zz"),
        "pub func ping() -> str { return \"pong\" }\n",
    );

    let (code, out, err) = run_zz(&d, &["install"]);
    assert_eq!(code, 0, "flagless install failed:\n{err}\n{out}");
    assert!(
        !d.join("vendor").join("helper").exists(),
        "disabled optional dep must not be linked into vendor/"
    );

    let (code, out, err) = run_zz(&d, &["install", "--features", "helpers"]);
    assert_eq!(code, 0, "feature-enabled install failed:\n{err}\n{out}");
    assert!(
        d.join("vendor").join("helper").exists(),
        "enabled optional dep must be linked into vendor/"
    );

    let (code, _, err) = run_zz(&d, &["install", "--features", "nope"]);
    assert_ne!(code, 0, "unknown feature must fail install");
    assert!(
        err.contains("unknown feature `nope`"),
        "install must name the unknown feature, got:\n{err}"
    );
}
