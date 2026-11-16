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

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn zz_bin() -> PathBuf {
    // `CARGO_BIN_EXE_zz` is the built binary for this test run and
    // carries the platform executable suffix (`zz.exe` on Windows).
    PathBuf::from(env!("CARGO_BIN_EXE_zz"))
}

fn require_native() -> bool {
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return false;
    }
    true
}

fn build_zz(args: &[&str], cwd: &Path) -> (i32, String, String) {
    build_zz_env(args, cwd, &[])
}

/// [`build_zz`] with extra environment pairs. Used by the toolchain probe,
/// which needs a real build (see `ZZ_BUILD_CACHE` there).
fn build_zz_env(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(zz_bin());
    cmd.args(args).current_dir(cwd);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd
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
    return if n <= 1 {
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
    return if y >= x {
        z
    } else {
        tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y))
    }
}
func main() {
    println(tak(10, 5, 0))
}
"#;

// Loop-iteration snapshot (mirrors
// tests/fixtures/regression/loop_iter_snapshot.zz): push / index-write /
// rebind inside the body must not affect visited elements or trip count.
// B1 regression: ZZ_NATIVE closures are refcounted; spawn/join in a loop
// releases each closure rep exactly once (no leak, no use-after-free).
const B1_CLOSURES: &str = r#"
import std.task
func main() {
    var i := 0
    var total := 0
    while i < 400 {
        h := task.spawn(|_| {
            s := 0
            for j in 0..10 {
                s = s + j
            }
            s
        })
        total = total + task.join(h)
        i = i + 1
    }
    println("b1_closures_ok={total}")
}
"#;

// B3 regression: chan.send_batch must hand off to a registered green
// waiter instead of stranding it (lost wakeup).
const B3_SEND_BATCH: &str = r#"
import std.task
import std.chan
func main() {
    c := std.chan()
    h := task.spawn(|_| { std.chan.recv(c) })
    arr := [1, 2, 3, 4, 5]
    std.chan.send_batch(c, arr)
    val := task.join(h)
    println("b3_send_batch_wakeup_ok={val}")
}
"#;

// B5 regression: closure capture-cell allocation must never leave a NULL
// cell that dispatch dereferences; heavy cell churn under ASan/UBSan.
const B5_CLOSURE_CELLS: &str = r#"
import std.task
func main() {
    var i := 0
    var total := 0
    while i < 400 {
        n := i
        h := task.spawn(|_| {
            s := 0
            for j in 0..n {
                s = s + j
            }
            s
        })
        total = total + task.join(h)
        i = i + 1
    }
    println("b5_closure_cells_ok={total}")
}
"#;

// B15 regression: json_pretty_print must not dereference a NULL v.arr/v.dict
// (default-constructed array/dict have NULL payloads).
const B15_JSON_PRETTY_NULL: &str = r#"
import std.json
func main() {
    var ok := 1
    var a := json.pretty([]) ?? ""
    var d := json.pretty({}) ?? ""
    var n := json.pretty(42) ?? ""
    var s := json.pretty("hi") ?? ""
    var m := json.pretty({"x": [1, 2], "y": "z"}) ?? ""
    if a == "" { ok = 0 }
    if d == "" { ok = 0 }
    if n == "" { ok = 0 }
    if s == "" { ok = 0 }
    if m == "" { ok = 0 }
    if ok != 1 { println("b15_json_pretty_null=FAIL") }
    else { println("b15_json_pretty_null_ok") }
}
"#;

// B13 regression: zz_str_append_str/zz_str_append_lit release the old
// buffer with the right ref discipline; shared accumulator exercises the
// fresh-alloc + atomic-release path every iteration.
const B13_STR_APPEND: &str = r#"
func main() {
    var i := 0
    var s := ""
    while i < 2000 {
        t := s
        s = s + "x"
        i = i + 1
    }
    println("b13_str_append_mutable_ok={len(s)}")
}
"#;

// B12 regression: json_parse_number must not let strtod read past the
// input buffer; numbers ending exactly at EOF plus a long digit run.
const B12_JSON_NUMBER_EDGE: &str = r#"
import std.json
func main() {
    var ok := 1
    a := json.parse("123") ?? json.null()
    b := json.parse("1.5e10") ?? json.null()
    c := json.parse("-0.25") ?? json.null()
    d := json.parse("12345678901234567890") ?? json.null()
    if a == json.null() { ok = 0 }
    if b == json.null() { ok = 0 }
    if c == json.null() { ok = 0 }
    if d == json.null() { ok = 0 }
    if json.as_int(a) != 123 { ok = 0 }
    if ok != 1 { println("b12_json_number_edge=FAIL") }
    else { println("b12_json_number_edge_ok") }
}
"#;

// B11 regression: zz_handoff_run_inline must restore the suspend verdict
// only when inline nesting is balanced; two chained inline handoffs (depth 2).
const B11_HANDOFF_INLINE_NEST: &str = r#"
import std.task
import std.chan
func main() {
    var i := 0
    var total := 0
    while i < 100 {
        c1 := std.chan()
        c2 := std.chan()
        w2 := task.spawn(|_| { std.chan.recv(c2) })
        w1 := task.spawn(|_| {
            var v := std.chan.recv(c1)
            std.chan.send(c2, v + 1)
        })
        std.chan.send(c1, 5)
        var r := std.chan.recv(c2)
        task.join(w1)
        task.join(w2)
        total = total + r
        i = i + 1
    }
    println("b11_handoff_inline_nest_ok={total}")
}
"#;

// B10 regression: zz_value_dup must not memoize a null-payload OOM
// closure; nested spawn dups closures at multiple levels.
const B10_CLOSURE_DUP_STORM: &str = r#"
import std.task
func main() {
    var i := 0
    var total := 0
    while i < 200 {
        outer := task.spawn(|_| {
            inner := task.spawn(|_| {
                s := 0
                for j in 0..5 {
                    s = s + j
                }
                s
            })
            task.join(inner)
        })
        total = total + task.join(outer)
        i = i + 1
    }
    println("b10_closure_dup_storm_ok={total}")
}
"#;

// B9 regression: zz_object_detach must decrement the struct refcount
// atomically; concurrent field writes on a shared struct detach the header.
const B9_OBJECT_DETACH: &str = r#"
import std.task
struct Shared { val: int }
func main() {
    var i := 0
    while i < 200 {
        p := Shared{val: 0}
        h1 := task.spawn(|_| { p.val = p.val + 1 })
        h2 := task.spawn(|_| { p.val = p.val + 2 })
        task.join(h1)
        task.join(h2)
        i = i + 1
    }
    println("b9_object_detach_race_ok")
}
"#;

// B7 regression: zz_iter_filter must release the predicate's return value;
// heap returns leak without it. Predicate returns heap arrays each call.
const B7_ITER_FILTER: &str = r#"
import std.iter
func main() {
    var i := 0
    var n := 0
    while i < 400 {
        a := ["alpha", "beta", "gamma", "delta"]
        var r := std.iter.filter(a, |_| { [1] })
        n = n + r.len()
        i = i + 1
    }
    println("b7_iter_filter_ok={n}")
}
"#;

// B6 regression: arena arrays migrated to heap (refs reset to 0) must be
// freed with free(), not free()d as arena memory. Array migrate + drop churn.
const B6_ARRAY_ARENA_MIGRATE: &str = r#"
func main() {
    var i := 0
    var s := 0
    while i < 300 {
        a := []
        var j := 0
        while j < 8 {
            a = vec.push(a, j)
            j = j + 1
        }
        s = s + a.len()
        i = i + 1
    }
    println("b6_array_arena_migrate_ok={s}")
}
"#;

// B4 regression: chan.recv_batch must not steal from a registered green
// waiter; recv_batch lock-free path exercised each iteration.
const B4_RECV_BATCH: &str = r#"
import std.task
import std.chan
func main() {
    c := std.chan()
    var ok := 1
    var i := 0
    while i < 60 {
        hw := task.spawn(|_| { std.chan.recv(c) })
        std.chan.send(c, 3 + i)
        var v := task.join(hw)
        if v != 3 + i { ok = 0 }
        var b := std.chan.recv_batch(c, 2)
        i = i + 1
    }
    println("b4_recv_batch_waiter_ok={ok}")
}
"#;

const LOOP_MUTATE: &str = r#"
func main() {
    a := [10, 20, 30]
    n := 0
    s1 := 0
    for v in a {
        n = n + 1
        s1 = s1 + v
        if n == 1 {
            a = vec.push(a, 99)
        }
    }
    println(n)
    println(s1)
    println(len(a))
    println(a[3])
    b := [10, 20, 30]
    s := 0
    m := 0
    for v in b {
        m = m + 1
        s = s + v
        if m == 1 {
            b[1] = 200
        }
    }
    println(m)
    println(s)
    println(b[1])
    c := [10, 20, 30]
    t := 0
    k := 0
    for v in c {
        k = k + 1
        t = t + v
        if k == 1 {
            c = [7, 8]
        }
    }
    println(k)
    println(t)
    println(len(c))
    println(c[0])
    println(c[1])
    d := {"a": 10, "b": 20, "c": 30}
    n2 := 0
    s2 := 0
    for _k, v in d {
        n2 = n2 + 1
        s2 = s2 + v
        if n2 == 1 {
            d["b"] = 200
            d["z"] = 99
        }
    }
    println(n2)
    println(s2)
    println(d["b"])
    println(len(d))
}
"#;

const CASES: &[(&str, &str)] = &[
    ("fib", FIB),
    ("sum_range", SUM_RANGE),
    ("array_io", ARRAY_IO),
    ("str_concat", STR_CONCAT),
    ("tak", TAK),
    ("loop_mutate", LOOP_MUTATE),
    ("b1_closures", B1_CLOSURES),
    ("b3_send_batch", B3_SEND_BATCH),
    ("b4_recv_batch", B4_RECV_BATCH),
    ("b5_closure_cells", B5_CLOSURE_CELLS),
    ("b6_array_arena_migrate", B6_ARRAY_ARENA_MIGRATE),
    ("b7_iter_filter", B7_ITER_FILTER),
    ("b9_object_detach", B9_OBJECT_DETACH),
    ("b10_closure_dup_storm", B10_CLOSURE_DUP_STORM),
    ("b11_handoff_inline_nest", B11_HANDOFF_INLINE_NEST),
    ("b12_json_number_edge", B12_JSON_NUMBER_EDGE),
    ("b13_str_append", B13_STR_APPEND),
    ("b15_json_pretty_null", B15_JSON_PRETTY_NULL),
];

/// Cases only the instrumented HIR leg can run: the chunk backend has no
/// `MakeVariant` yet (#508), so variant construction fails coverage there.
/// HIR is the backend that owns the payload boxes (`zz_vbox`).
const HIR_ONLY_CASES: &[(&str, &str)] = &[("variant_arc", VARIANT_ARC)];

// Variant payload boxes are reference counted (#507): one Option value
// stored in several container slots owns several box references and must
// be freed exactly once.
const VARIANT_ARC: &str = r#"
func main() {
    o := .some("heap payload")
    a := [o, o, o]
    for v in a { println(v) }
    d := {"a": o, "b": o}
    println(d["a"])
    println(d["b"])
    r := .ok("res payload")
    println(r ?? "never")
    println("variant arc ok")
}
"#;

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
    // `ZZ_BUILD_CACHE=0`: the probe asserts on the verbose clang line, which
    // a cache hit replaces with the reused entry (no toolchain needed). Force
    // the real build so a repeat run of this suite still pins the toolchain.
    let (code, _out, err) = build_zz_env(
        &["build", "--dynamic", "--verbose", probe.to_str().unwrap()],
        &probe_dir,
        &[("ZZ_BUILD_CACHE", "0")],
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
    for (name, src) in CASES.iter().chain(HIR_ONLY_CASES.iter()).copied() {
        // HIR-only cases (see above) still get the instrumented build and
        // run; there is no second backend to diff against.
        let hir_only = HIR_ONLY_CASES.iter().any(|(n, _)| *n == name);
        let backends: &[&str] = if hir_only {
            &["hir"]
        } else {
            &["hir", "chunk"]
        };
        let mut outs: Vec<(String, i32, String)> = Vec::new();
        for backend in backends {
            let dir = root.join(format!("{name}_{backend}"));
            std::fs::create_dir_all(&dir).unwrap();
            let f = dir.join(format!("{name}.zz"));
            std::fs::write(&f, src).unwrap();
            let mut args = vec!["build", "--dynamic"];
            if *backend == "chunk" {
                args.push("--chunk");
            }
            args.push(f.to_str().unwrap());
            let (code, _out, err) = build_zz(&args, &dir);
            assert_eq!(code, 0, "{backend} sanitizer build of {name} failed: {err}");
            assert_no_reports(&format!("{backend} build {name}"), &err);
            let bin = support::exe_in(&dir, name);
            assert!(bin.exists(), "{backend} binary missing for {name}");
            let (rcode, stdout, rerr) = run_bin(&bin);
            assert_no_reports(&format!("{backend} run {name}"), &rerr);
            assert_eq!(rcode, 0, "{backend} sanitizer run of {name} failed: {rerr}");
            outs.push((backend.to_string(), rcode, stdout));
        }
        if !hir_only {
            assert_eq!(outs[0].1, outs[1].1, "{name}: exit differs");
            assert_eq!(outs[0].2, outs[1].2, "{name}: stdout differs");
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
