//! ASan+UBSan AOT leg: every program in the corpus must build clean under
//! `zz build` (HIR→C) and `zz build --chunk` (Chunk→C) with
//! `ZZ_SANITIZE=address,undefined` and run with exit 0 + identical stdout.
//!
//! Sanitizer reports are fatal (`-fno-sanitize-recover=all` is baked into
//! the flags when `ZZ_SANITIZE` is set), so exit 0 == clean: any OOB,
//! use-after-free, UB, or leak fails the case. The instrumented C runtime
//! archive carries its own cache key, so sanitizer builds never poison
//! (or reuse) plain archives.
//!
//! The leg also pins + logs the toolchain: when `ZZ_CLANG_VERSION` is set
//! (CI), the `zz build --verbose` line must name that major version, and
//! the exact version string is printed (visible with `--nocapture` and on
//! failure). Locally the pin is unset and any detected provider works.
//!
//! Corpus mirrors `chunk_aot_diff` at small scale (sanitizers slow runs
//! ~2-4x; timing is not asserted here, only cleanliness + parity).

use std::path::{Path, PathBuf};
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
    true
}

fn build_zz(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let out = Command::new(zz_bin())
        .args(args)
        .current_dir(cwd)
        // Hermetic: the sanitizer set travels with the test, never with
        // ambient CI env (which must stay clean for the other legs).
        .env("ZZ_SANITIZE", "address,undefined")
        // Leak detection off: chunk range boxes are immortal by design
        // (each `MakeRange` leaks one box; #254), so LSan would fail
        // every range loop. The leg hunts errors + UB, not exit leaks;
        // unboxed range loops (arraysum/sieve slices) remove the box
        // — and the leak — for hot loops anyway.
        .env("ASAN_OPTIONS", "halt_on_error=1:detect_leaks=0")
        .env("UBSAN_OPTIONS", "halt_on_error=1:print_stacktrace=1")
        .output()
        .expect("zz binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_bin(bin: &Path) -> (i32, String, String) {
    let out = Command::new(bin)
        // See build_zz: leaks are out of scope for this leg (#254).
        .env("ASAN_OPTIONS", "halt_on_error=1:detect_leaks=0")
        .env("UBSAN_OPTIONS", "halt_on_error=1:print_stacktrace=1")
        .output()
        .expect("binary should run");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Fail on any sanitizer report text even when the exit code slipped
/// through (belt and braces: reports are fatal by flags, but a harness
/// that only trusts exit codes is one flag regression from silent).
fn assert_no_reports(where_: &str, stderr: &str) {
    for marker in [
        "ERROR: AddressSanitizer",
        "runtime error:",
        "SUMMARY: AddressSanitizer",
        "SUMMARY: UndefinedBehaviorSanitizer",
    ] {
        assert!(
            !stderr.contains(marker),
            "{where_}: sanitizer report leaked:\n{stderr}"
        );
    }
}

const FIB: &str = r#"
func fib(n: int) -> int {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}
func main() {
    println(fib(20))
}
"#;

const SUM_RANGE: &str = r#"
func main() {
    s := 0
    for i in 0..20000 {
        if i % 2 == 0 {
            s = s + i
        } else {
            s = s - 1
        }
    }
    println(s)
}
"#;

const ARRAY_IO: &str = r#"
func main() {
    a := []
    for i in 0..2000 {
        a = vec.push(a, i % 97)
    }
    a[0] = 42
    a[1999] = -1
    s := 0
    for v in a {
        s = s + v
    }
    println(a[0])
    println(a[1999])
    println(s)
}
"#;

const STR_CONCAT: &str = r#"
func main() {
    s := ""
    for i in 0..500 {
        s = s + "a"
    }
    println(len(s))
}
"#;

const TAK: &str = r#"
func tak(x: int, y: int, z: int) -> int {
    if y >= x {
        z
    } else {
        tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y))
    }
}
func main() {
    println(tak(10, 5, 0))
}
"#;

const CASES: &[(&str, &str)] = &[
    ("fib", FIB),
    ("sum_range", SUM_RANGE),
    ("array_io", ARRAY_IO),
    ("str_concat", STR_CONCAT),
    ("tak", TAK),
];

/// Parse the clang major version out of a `zz build --verbose` stderr
/// line (`zz: clang <version...> <flags...>`, where `<version...>` is
/// `clang version 19.1.2 (...)` for system clang). Returns `None` for
/// non-clang providers (zig) — the pin only constrains clang.
fn parse_clang_major(verbose_stderr: &str) -> Option<String> {
    for line in verbose_stderr.lines() {
        let Some(rest) = line.strip_prefix("zz: clang ") else {
            continue;
        };
        // Vendor infixes exist (`Ubuntu clang version 23.1.2 (...)`,
        // `Apple clang version ...`): scan for the `clang version X`
        // triple anywhere in the line, not just at the start.
        let words: Vec<&str> = rest.split_whitespace().collect();
        for w in words.windows(3) {
            if w[0] == "clang" && w[1] == "version" {
                return w[2].split('.').next().map(str::to_string);
            }
        }
    }
    None
}

#[test]
fn clang_major_parses_plain_and_vendor_strings() {
    // Plain `clang version X` (local toolchains).
    assert_eq!(
        parse_clang_major("zz: clang clang version 23.1.1 -fwrapv"),
        Some("23".to_string())
    );
    // Vendor infix (`apt.llvm.org` Ubuntu build) with leading noise
    // lines: the old parser aborted on the first non-matching line
    // (`?` on `strip_prefix`) and required the pair at word 0.
    assert_eq!(
        parse_clang_major(
            "building /tmp/x/probe.zz (dev)\nzz: clang Ubuntu clang version 23.1.2 (++20260919103626+4b1925210476-1~exp1~20260919223755.77) -O3"
        ),
        Some("23".to_string())
    );
    assert_eq!(parse_clang_major("nothing to parse here"), None);
}

#[test]
fn sanitizer_aot_hir_and_chunk() {
    if !require_native() {
        return;
    }
    // One verbose build first: pins + logs the toolchain before the
    // (slower) instrumented builds run.
    let root = std::env::temp_dir().join(format!("zz-san-{}", std::process::id()));
    let probe_dir = root.join("probe");
    std::fs::create_dir_all(&probe_dir).unwrap();
    let probe = probe_dir.join("probe.zz");
    std::fs::write(&probe, "func main() {\n println(1)\n}\n").unwrap();
    let (code, _out, err) = build_zz(
        &["build", "--dynamic", "--verbose", probe.to_str().unwrap()],
        &probe_dir,
    );
    assert_eq!(code, 0, "sanitizer probe build failed: {err}");
    assert!(
        err.contains("-fsanitize=address,undefined"),
        "sanitizer flags missing from build flags:\n{err}"
    );
    eprintln!("sanitizer leg toolchain: {}", {
        err.lines()
            .find(|l| l.starts_with("zz: clang "))
            .unwrap_or("zz: clang <version not logged>")
    });
    if let Ok(pin) = std::env::var("ZZ_CLANG_VERSION") {
        let pin = pin.trim().to_string();
        if !pin.is_empty() {
            match parse_clang_major(&err) {
                Some(major) => assert_eq!(
                    major, pin,
                    "clang pin mismatch: want major {pin}, verbose says:\n{err}"
                ),
                None => panic!("could not parse clang version from verbose build:\n{err}"),
            }
        }
    }
    for (name, src) in CASES {
        let mut outs: Vec<(String, i32, String)> = Vec::new();
        for backend in ["hir", "chunk"] {
            let dir = root.join(format!("{name}_{backend}"));
            std::fs::create_dir_all(&dir).unwrap();
            let f = dir.join(format!("{name}.zz"));
            std::fs::write(&f, src).unwrap();
            let mut args = vec!["build", "--dynamic"];
            if backend == "chunk" {
                args.push("--chunk");
            }
            args.push(f.to_str().unwrap());
            let (code, _out, err) = build_zz(&args, &dir);
            assert_eq!(code, 0, "{backend} sanitizer build of {name} failed: {err}");
            assert_no_reports(&format!("{backend} build {name}"), &err);
            let bin = dir.join(format!("bin/{name}"));
            assert!(bin.exists(), "{backend} binary missing for {name}");
            let (rcode, stdout, rerr) = run_bin(&bin);
            assert_no_reports(&format!("{backend} run {name}"), &rerr);
            assert_eq!(rcode, 0, "{backend} sanitizer run of {name} failed: {rerr}");
            outs.push((backend.to_string(), rcode, stdout));
        }
        assert_eq!(outs[0].1, outs[1].1, "{name}: exit differs");
        assert_eq!(outs[0].2, outs[1].2, "{name}: stdout differs");
    }
    let _ = std::fs::remove_dir_all(&root);
}
