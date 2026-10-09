//! Per-module translation units (`ZZ_CODEGEN_UNITS=1`): split-TU binaries
//! must behave identically to single-TU ones. The fixture exercises what
//! partitioning can break: cross-module calls, module globals + top-level
//! init, closures, and scalar twin-eligible functions.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const MAIN_ZZ: &str = r#"import helper

startup := "main-init"

func triple(x: int) -> int {
    x * 3
}

func main() {
    println(helper.get_greeting())
    println(startup)
    println(helper.add1(41))
    println(triple(14))
    inc := |x| x + 1
    println(inc(9))
    println("units_ok")
}
"#;

const HELPER_ZZ: &str = r#"pub func greet(name: str) -> str {
    "hi {name}"
}

pub func add1(x: int) -> int {
    x + 1
}

greeting := greet("init")

pub func get_greeting() -> str {
    greeting
}
"#;

/// Fresh temp project dir with main.zz + helper.zz.
fn temp_project() -> PathBuf {
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("zz-units-e2e-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmpdir");
    std::fs::write(dir.join("main.zz"), MAIN_ZZ).expect("fixture");
    std::fs::write(dir.join("helper.zz"), HELPER_ZZ).expect("fixture");
    dir
}

fn zz() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn build(dir: &Path, units: bool) -> PathBuf {
    let out = dir.join(if units { "app_units" } else { "app_single" });
    let mut cmd = Command::new(zz());
    cmd.args(["build", "main.zz", "-o"])
        .arg(&out)
        .current_dir(dir);
    if units {
        cmd.env("ZZ_CODEGEN_UNITS", "1");
    }
    let res = cmd.output().expect("exec zz build");
    assert!(
        res.status.success(),
        "build (units={units}) failed: {}",
        String::from_utf8_lossy(&res.stderr)
    );
    out
}

fn run_bin(bin: &Path) -> (i32, String) {
    let out = Command::new(bin).output().expect("exec app");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn units_split_matches_single_tu() {
    let dir = temp_project();
    let single = build(&dir, false);
    let split = build(&dir, true);
    let (c0, o0) = run_bin(&single);
    let (c1, o1) = run_bin(&split);
    assert_eq!(c0, 0, "single-TU exit");
    assert_eq!(c1, 0, "split-TU exit");
    assert!(o0.contains("units_ok"), "single-TU output sanity: {o0:?}");
    assert_eq!(o0, o1, "split-TU output must match single-TU");
    let _ = std::fs::remove_dir_all(&dir);
}
