//! RSS leak gate: AOT binaries must not grow without bound when loop
//! bodies allocate and drop. Complements the ASan leg, which runs with
//! `detect_leaks=0` (range boxes are immortal by design, #254) and is
//! therefore blind to genuine leaks in new emitter code.
//!
//! Method: build a churn program (2000 outer iterations, each building
//! then dropping a 10k-element array) with `-p` on both backends and
//! run each under `ulimit -v` (address-space cap). Steady-state use is
//! a few MB; the cap is 512MB, so only unbounded growth trips it (the
//! kernel kills past the cap → nonzero exit → failure). Stdout must
//! still match exactly (correctness and leak-freedom in one gate).
//!
//! The churn program nests `for`-in-`for`, so it also dogfoods #311 on
//! the chunk backend.

use std::path::PathBuf;
use std::process::Command;

fn zz_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let debug = deps.parent().unwrap();
    debug.join("zz")
}

fn require_native() -> bool {
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return false;
    }
    // Address-space caps need bash + ulimit (Linux CI and dev boxes).
    if cfg!(not(target_os = "linux")) {
        eprintln!("skip: RSS cap needs Linux ulimit");
        return false;
    }
    true
}

// 2000 generations x 10k pushes; steady-state holds one 10k array
// (~160KB). Expected sum: 2000 * 10000.
const CHURN: &str = r#"
func main() {
    s := 0
    for r in 0..2000 {
        a := []
        for i in 0..10000 {
            a = vec.push(a, i % 97)
        }
        s = s + len(a)
    }
    println(s)
}
"#;

const EXPECTED: &str = "20000000\n";
// 512MB address-space cap: ~3000x steady-state. Only a per-iteration
// leak (the old loop-result placeholder class, or a new emitter leak)
// can reach it in 2000 generations.
const CAP_KB: &str = "524288";

fn build(dir: &std::path::Path, backend: &str, extra: &[&str]) {
    let f = dir.join("churn.zz");
    std::fs::write(&f, CHURN).unwrap();
    let mut args = vec!["build", "-p"];
    args.extend(extra);
    args.push(f.to_str().unwrap());
    let out = Command::new(zz_bin())
        .args(&args)
        .current_dir(dir)
        .output()
        .expect("zz build should run");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "{backend} build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_capped(dir: &std::path::Path, backend: &str) -> (i32, String) {
    let bin = dir.join("bin/churn");
    assert!(bin.exists(), "{backend} binary missing");
    // `ulimit -v` applies to the shell's children; exec replaces the
    // shell so the cap binds exactly the test binary.
    let script = format!("ulimit -v {CAP_KB}; exec \"{}\"", bin.display());
    let out = Command::new("bash")
        .args(["-c", script.as_str()])
        .output()
        .expect("bash should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn rss_no_unbounded_growth() {
    if !require_native() {
        return;
    }
    let root = std::env::temp_dir().join(format!("zz-rss-{}", std::process::id()));
    let mut outs = Vec::new();
    for (backend, extra) in [("hir", &[][..]), ("chunk", &["--chunk"][..])] {
        let dir = root.join(format!("churn_{backend}"));
        std::fs::create_dir_all(&dir).unwrap();
        build(&dir, backend, extra);
        let (code, stdout) = run_capped(&dir, backend);
        assert_eq!(
            code, 0,
            "{backend}: exited {code} under a 512MB address-space cap (leak?)"
        );
        outs.push(stdout);
    }
    for (i, out) in outs.iter().enumerate() {
        assert_eq!(
            *out, EXPECTED,
            "backend {i}: wrong sum (correctness broke under the cap): {out:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}
