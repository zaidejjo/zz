//! Phased `zz build` output (#629).
//!
//! `zz build` renders five numbered, generic steps — `Checking`,
//! `Optimizing`, `Compiling`, `Linking`, `Publishing` — with a header that
//! names the target (package for a project, file for a loose source). Piped
//! stderr gets exactly one plain line per step; `ZZ_COLOR=always` forces
//! ANSI through a pipe.
//!
//! Every test skips on hosts without the native backend / a clang, like
//! `build_e2e.rs`.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn zz() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn temp_dir(tag: &str) -> PathBuf {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "zz-build-phases-{tag}-{}-{uniq}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmpdir");
    dir
}

/// A program unique to this test run, so a leftover build-cache entry from
/// an earlier run can never turn the "cold build" test into a cache hit.
fn unique_prog(tag: &str) -> String {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("println(\"phase_{tag}_{}_{}\")\n", std::process::id(), uniq)
}

/// Run `zz` in `dir`. Inherited colour variables are cleared so a
/// developer's `ZZ_COLOR` / `NO_COLOR` cannot change the assertions.
fn run(dir: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(zz());
    cmd.args(args)
        .current_dir(dir)
        .env_remove("ZZ_COLOR")
        .env_remove("NO_COLOR");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("exec zz");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Skip when the host cannot build natively (Windows / `ZZ_SKIP_NATIVE`).
fn unsupported() -> bool {
    if support::native_unsupported() {
        eprintln!("skip: native backend unsupported on this host");
        return true;
    }
    false
}

/// True when the failed build failed only because no clang is installed.
fn missing_clang(code: i32, stderr: &str) -> bool {
    code != 0 && stderr.contains("no clang found")
}

#[test]
fn cold_build_prints_five_steps_in_order() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("steps");
    std::fs::write(dir.join("hello.zz"), unique_prog("steps")).unwrap();
    let (code, stdout, stderr) = run(&dir, &["build", "hello.zz"], &[]);
    if missing_clang(code, &stderr) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(
        code, 0,
        "build failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The five steps appear once each, in pipeline order, and the labels are
    // the generic user-facing set.
    let mut last = 0usize;
    for (i, label) in [
        "Checking",
        "Optimizing",
        "Compiling",
        "Linking",
        "Publishing",
    ]
    .iter()
    .enumerate()
    {
        let needle = format!("==> [{}/5] {label}", i + 1);
        let at = stderr
            .find(&needle)
            .unwrap_or_else(|| panic!("missing `{needle}` in:\n{stderr}"));
        assert!(at >= last, "step {label} out of order:\n{stderr}");
        last = at;
    }
    // No pipeline internals leak into user-facing text.
    for banned in ["HIR", "lowering", "DCE", "chunk", "translation unit"] {
        assert!(
            !stderr.contains(banned),
            "`{banned}` leaked into build output:\n{stderr}"
        );
    }
    // The existing success line (and its `(static,` marker) is preserved.
    assert!(
        stderr.contains("built ") && stderr.contains("(static,"),
        "success line missing:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn project_build_names_package_in_header() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("pkg");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("zz.toml"),
        "[package]\nname = \"demopkg\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.zz"), unique_prog("pkg")).unwrap();
    let (code, stdout, stderr) = run(&dir, &["build", "src/main.zz"], &[]);
    if missing_clang(code, &stderr) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(
        code, 0,
        "project build failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("building demopkg (static)"),
        "header must name the package:\n{stderr}"
    );
    assert!(
        !stderr.contains("building src/main.zz"),
        "header must not name an internal path:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn standalone_build_names_file_in_header() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("loose");
    std::fs::write(dir.join("hello.zz"), unique_prog("loose")).unwrap();
    let (code, stdout, stderr) = run(&dir, &["build", "hello.zz"], &[]);
    if missing_clang(code, &stderr) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(
        code, 0,
        "loose build failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("building hello.zz (static)"),
        "header must name the file:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn second_build_reports_cache_hit() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("cache");
    std::fs::write(dir.join("hello.zz"), unique_prog("cache")).unwrap();
    let (code, _, first) = run(&dir, &["build", "hello.zz"], &[]);
    if missing_clang(code, &first) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(code, 0, "first build failed:\n{first}");
    let (code, stdout, second) = run(&dir, &["build", "--verbose", "hello.zz"], &[]);
    assert_eq!(
        code, 0,
        "cached build failed.\nstdout:\n{stdout}\nstderr:\n{second}"
    );
    // The probe runs before the frontend: unchanged source skips steps 1-4
    // entirely and only publishes.
    assert!(
        second.contains("cached · source unchanged"),
        "cache hit must be reported honestly:\n{second}"
    );
    // `--verbose` names what was reused (there is no clang line to print on
    // a hit: a cached build never touches a toolchain).
    assert!(
        second.contains("zz: cache hit "),
        "a verbose cache hit must name the reused entry:\n{second}"
    );
    assert!(
        !second.contains("[1/5]"),
        "a cache hit must not run the frontend:\n{second}"
    );
    assert!(
        second.contains("[5/5] Publishing"),
        "the published binary is still named:\n{second}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn steps_never_touch_stdout() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("stdout");
    std::fs::write(dir.join("hello.zz"), unique_prog("stdout")).unwrap();
    let (code, stdout, stderr) = run(&dir, &["build", "hello.zz"], &[]);
    if missing_clang(code, &stderr) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(code, 0, "build failed:\n{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout must stay a single line, got:\n{stdout:?}"
    );
    assert!(
        lines[0].starts_with("built ") && lines[0].ends_with("hello"),
        "stdout must be the published path:\n{stdout:?}"
    );
    assert!(
        !stdout.contains("[1/5]"),
        "steps must never reach stdout:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_error_marks_step_one_failed() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("err");
    std::fs::write(
        dir.join("bad.zz"),
        "func main() {\n    x := missing_thing + 1\n    println(x)\n}\n",
    )
    .unwrap();
    let (code, stdout, stderr) = run(&dir, &["build", "bad.zz"], &[]);
    if missing_clang(code, &stderr) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_ne!(code, 0, "a type error must fail the build:\n{stderr}");
    assert!(
        stderr.contains("==> [1/5] Checking: failed"),
        "step 1 must be closed as failed:\n{stderr}"
    );
    assert!(
        !stderr.contains("[2/5]"),
        "no later step may start after a failure:\n{stderr}"
    );
    assert!(
        stderr.contains("missing_thing"),
        "the diagnostic must still be rendered:\n{stderr}"
    );
    assert!(
        !stdout.contains("built "),
        "nothing is published on failure:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn color_env_controls_output() {
    if unsupported() {
        return;
    }
    let dir = temp_dir("color");
    std::fs::write(dir.join("hello.zz"), unique_prog("color")).unwrap();
    let (code, _, plain) = run(&dir, &["build", "hello.zz"], &[]);
    if missing_clang(code, &plain) {
        eprintln!("skip: no clang on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(code, 0, "build failed:\n{plain}");
    assert!(
        !plain.contains("\x1b["),
        "piped output must stay plain by default:\n{plain:?}"
    );

    // `always` forces ANSI through a pipe (CI logs, docs captures).
    let (code, _, colored) = run(
        &dir,
        &["build", "hello.zz"],
        &[("ZZ_COLOR", "always"), ("TERM", "xterm-256color")],
    );
    assert_eq!(code, 0, "coloured build failed:\n{colored}");
    assert!(
        colored.contains("\x1b["),
        "ZZ_COLOR=always must emit ANSI:\n{colored:?}"
    );

    // `never` is the one-variable kill switch.
    let (code, _, off) = run(
        &dir,
        &["build", "hello.zz"],
        &[("ZZ_COLOR", "never"), ("TERM", "xterm-256color")],
    );
    assert_eq!(code, 0, "plain build failed:\n{off}");
    assert!(
        !off.contains("\x1b["),
        "ZZ_COLOR=never must emit no ANSI:\n{off:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
