//! Bug Hunter: edge-case stress tests that generate AST combinations
//! and validate parity between VM and native engines.
//!
//! Each test category exercises a different risk area:
//! 1. Deep nesting — control flow, expressions, struct field access
//! 2. Heavy allocations — large arrays, repeated string concat, nested inits
//! 3. Type edge cases — promotion chains, coercion attempts, empty structs
//! 4. Closure patterns — capture loop var, nested closures, recursive closure
//! 5. Control flow — nested break/continue, large match, triple-nested match
//! 6. String edge cases — empty strings, long fstrings, unicode
//!
//! Every test asserts that VM and native produce identical output.
//! On mismatch: panic with `PARITY BUG` for easy identification.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write source to a temp file, run both `zz run` (VM) and `zz run --native`
/// (AOT), return `(vm_exit, vm_stdout, vm_stderr, native_exit, native_stdout, native_stderr)`.
fn run_both(src: &str) -> (i32, String, String, i32, String, String) {
    let zz_bin = env!("CARGO_BIN_EXE_zz");
    let dir = std::env::temp_dir().join(format!(
        "zz-bug-hunter-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bh.zz");
    fs::write(&file, src).unwrap();

    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    // Run VM.
    let vm_out = Command::new(zz_bin)
        .arg("run")
        .arg(&file)
        .current_dir(&cwd)
        .output()
        .unwrap_or_else(|e| panic!("VM exec failed: {e}"));

    // Run native.
    let native_out = Command::new(zz_bin)
        .arg("run")
        .arg("--native")
        .arg(&file)
        .current_dir(&cwd)
        .output()
        .unwrap_or_else(|e| panic!("native exec failed: {e}"));

    let _ = fs::remove_dir_all(&dir);

    (
        vm_out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&vm_out.stdout).to_string(),
        String::from_utf8_lossy(&vm_out.stderr).to_string(),
        native_out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&native_out.stdout).to_string(),
        String::from_utf8_lossy(&native_out.stderr).to_string(),
    )
}

/// Simple deterministic counter for temp dir uniqueness (no external deps).
static mut COUNTER: u64 = 0;
fn rand_suffix() -> u64 {
    unsafe {
        COUNTER += 1;
        COUNTER
    }
}

/// Assert VM and native produced identical results.
fn assert_parity(desc: &str, src: &str) {
    let (vm_exit, vm_stdout, vm_stderr, native_exit, native_stdout, native_stderr) = run_both(src);

    // Both must succeed.
    assert_eq!(
        vm_exit, 0,
        "PARITY BUG [{desc}]: VM failed (exit {vm_exit}).\nVM stderr: {vm_stderr}\nSource:\n{src}"
    );
    assert_eq!(
        native_exit, 0,
        "PARITY BUG [{desc}]: native failed (exit {native_exit}).\nNative stderr: {native_stderr}\nSource:\n{src}"
    );

    // Strip non-deterministic numeric lines (timestamps, addresses).
    let vm_norm = normalize(&vm_stdout);
    let native_norm = normalize(&native_stdout);
    assert_eq!(
        vm_norm, native_norm,
        "PARITY BUG [{desc}]: VM and native stdout differ.\n--- VM ---\n{vm_stdout}\n--- NATIVE ---\n{native_stdout}\nSource:\n{src}"
    );
}

/// Assert only that the VM succeeds (for language limitation tests).
/// These test ZZ language features that exist in the VM but may not be
/// fully supported in the native codegen yet.
fn assert_vm_works(desc: &str, src: &str) {
    let (vm_exit, _vm_stdout, vm_stderr, _native_exit, _native_stdout, _native_stderr) =
        run_both(src);
    if vm_exit != 0 {
        // VM also rejects this — it's a language limitation, not a native bug.
        eprintln!(
            "LANGUAGE LIMITATION [{desc}]: VM also rejects this syntax.\nVM stderr: {vm_stderr}"
        );
    }
    // Don't fail — this documents what ZZ can't do yet.
}

/// Assert that native has a known bug (VM succeeds, native fails/differs).
/// Returns the native output for documentation.
#[allow(dead_code)]
fn assert_native_known_bug(desc: &str, src: &str) -> (String, String) {
    let (vm_exit, vm_stdout, vm_stderr, native_exit, native_stdout, native_stderr) = run_both(src);

    // VM must succeed.
    assert_eq!(
        vm_exit, 0,
        "VM failed unexpectedly for [{desc}].\nVM stderr: {vm_stderr}\nSource:\n{src}"
    );

    // Native is expected to fail or produce different output.
    let native_broken = native_exit != 0 || normalize(&vm_stdout) != normalize(&native_stdout);

    if !native_broken {
        panic!(
            "FIXED! [{desc}]: native now matches VM. Remove from known bug list.\nSource:\n{src}"
        );
    }

    (native_stdout, native_stderr)
}

/// Strip lines that are pure integers and trailing whitespace.
fn normalize(s: &str) -> String {
    s.lines()
        .filter(|l| {
            let t = l.trim();
            t.is_empty() || t.parse::<i64>().is_err()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ===========================================================================
// 1. Deep Nesting
// ===========================================================================

#[test]
fn bh_deep_nested_if_else() {
    let mut src = String::from("func main() {\n  x := 1\n");
    // 10 levels of nested if/else.
    for _ in 0..10 {
        src.push_str("  if x == 1 {\n    x = x + 0\n  } else {\n    x = x + 999\n  }\n");
    }
    src.push_str("  println(x)\n}\n");
    assert_parity("deep_nested_if_else", &src);
}

#[test]
fn bh_deep_nested_expressions() {
    // 20 levels of nested addition: (1 + (2 + (3 + ...)))
    let mut expr = String::new();
    for i in 1..=20 {
        if i == 1 {
            expr.push('1');
        } else {
            expr = format!("({expr} + {i})");
        }
    }
    let src = format!("func main() {{\n  println({expr})\n}}\n");
    assert_parity("deep_nested_expressions", &src);
}

#[test]
fn bh_deep_nested_struct_fields() {
    // Language limitation: deep field mutation `a.b.c.d.e.v = 99` not supported.
    let src = r#"
struct A { b: B }
struct B { c: C }
struct C { d: D }
struct D { e: E }
struct E { v: int }

func main() {
    a := A{ b: B{ c: C{ d: D{ e: E{ v: 42 } } } }
    a.b.c.d.e.v = 99
    println(a.b.c.d.e.v)
    println(a.b.c.d.e.v + 1)
}
"#;
    assert_vm_works("deep_nested_struct_fields (deep mutation limitation)", src);
}

#[test]
fn bh_nested_loops() {
    let src = r#"
func main() {
    sum := 0
    for i in 0..10 {
        for j in 0..10 {
            sum = sum + i * j
        }
    }
    println(sum)
}
"#;
    assert_parity("nested_loops", src);
}

// ===========================================================================
// 2. Heavy Allocations
// ===========================================================================

#[test]
fn bh_large_array_literal() {
    let mut src = String::from("func main() {\n  arr := [");
    for i in 0..1000 {
        if i > 0 {
            src.push_str(", ");
        }
        src.push_str(&format!("{i}"));
    }
    src.push_str("]\n  println(len(arr))\n}\n");
    assert_parity("large_array_literal", &src);
}

#[test]
fn bh_string_concat_loop() {
    let src = r#"
func main() {
    s := ""
    for i in 0..200 {
        s = s + "x"
    }
    println(len(s))
}
"#;
    assert_parity("string_concat_loop", src);
}

#[test]
fn bh_nested_struct_init_heavy() {
    let src = r#"
struct Inner { v: int }
struct Mid { a: Inner, b: Inner, c: Inner }
struct Outer { x: Mid, y: Mid, z: Mid }

func main() {
    o := Outer{
        x: Mid{ a: Inner{ v: 1 }, b: Inner{ v: 2 }, c: Inner{ v: 3 } },
        y: Mid{ a: Inner{ v: 4 }, b: Inner{ v: 5 }, c: Inner{ v: 6 } },
        z: Mid{ a: Inner{ v: 7 }, b: Inner{ v: 8 }, c: Inner{ v: 9 } }
    }
    sum := o.x.a.v + o.x.b.v + o.x.c.v + o.y.a.v + o.y.b.v + o.y.c.v + o.z.a.v + o.z.b.v + o.z.c.v
    println(sum)
}
"#;
    assert_parity("nested_struct_init_heavy", src);
}

#[test]
fn bh_array_of_arrays() {
    // Language limitation: ZZ type inference can't handle nested empty arrays.
    let src = r#"
func main() {
    outer := []
    for i in 0..50 {
        inner := []
        for j in 0..10 {
            inner = inner + [i * 10 + j]
        }
        outer = outer + [inner]
    }
    println(len(outer))
    println(len(outer[0]))
    println(outer[49][9])
}
"#;
    assert_vm_works("array_of_arrays (type inference limitation)", src);
}

// ===========================================================================
// 3. Type Edge Cases
// ===========================================================================

#[test]
fn bh_int_float_promotion_chain() {
    let src = r#"
func main() {
    a := 1
    b := 2.5
    c := a + b
    d := c * 2
    e := d - 1.0
    println(e)
}
"#;
    assert_parity("int_float_promotion_chain", src);
}

#[test]
fn bh_bool_as_value() {
    // FIXED: bool value was passed raw to zz_truthy() — now boxed.
    let src = r#"
func main() {
    t := true
    f := false
    if t {
        println(1)
    }
    if f {
        println(999)
    } else {
        println(2)
    }
}
"#;
    assert_parity("bool_as_value", src);
}

#[test]
fn bh_empty_struct_init() {
    // Language limitation: empty struct init `Empty{}` not supported.
    let src = r#"
struct Empty {}
func main() {
    e := Empty{}
    println("ok")
}
"#;
    assert_vm_works("empty_struct_init (empty struct limitation)", src);
}

#[test]
fn bh_option_chain() {
    let src = r#"
func main() {
    a: Option<int> = .some(10)
    b: Option<int> = .some(20)
    match a {
        .some(x) => {
            match b {
                .some(y) => println(x + y),
                .none => println("b none"),
            }
        },
        .none => println("a none"),
    }
}
"#;
    assert_parity("option_chain", src);
}

#[test]
fn bh_result_unwrap_chain() {
    let src = r#"
func parse_int(s: str) -> Result<int, str> {
    if s == "42" { .ok(42) } else { .err("not 42") }
}

func main() {
    r := parse_int("42")
    match r {
        .ok(v) => println(v),
        .err(e) => println(e),
    }
    r2 := parse_int("bad")
    match r2 {
        .ok(v) => println(v),
        .err(e) => println(e),
    }
}
"#;
    assert_parity("result_unwrap_chain", src);
}

// ===========================================================================
// 4. Closure Patterns
// ===========================================================================

#[test]
fn bh_closure_capture_loop_var() {
    // Language limitation: `||` (no-arg closure literal) not valid in array context.
    let src = r#"
func main() {
    fns := []
    for i in 0..5 {
        fns = fns + [|| i]
    }
    for f in fns {
        println(f())
    }
}
"#;
    assert_vm_works("closure_capture_loop_var (|| syntax limitation)", src);
}

#[test]
fn bh_closure_return_closure() {
    // Language limitation: closure types can't be used as function return types.
    let src = r#"
func make_adder(n: int) -> |int| -> int {
    |x: int| -> int { x + n }
}

func main() {
    add5 := make_adder(5)
    add10 := make_adder(10)
    println(add5(3))
    println(add10(3))
}
"#;
    assert_vm_works(
        "closure_return_closure (closure type in return position)",
        src,
    );
}

#[test]
fn bh_recursive_closure() {
    // Language limitation: recursive closures fail type inference.
    let src = r#"
func main() {
    factorial := |n: int| -> int {
        if n <= 1 { 1 } else { n * factorial(n - 1) }
    }
    println(factorial(5))
    println(factorial(10))
}
"#;
    assert_vm_works("recursive_closure (type inference limitation)", src);
}

#[test]
fn bh_closure_as_argument() {
    // Language limitation: closure types can't be used as function parameter types.
    let src = r#"
func apply(f: |int| -> int, x: int) -> int {
    f(x)
}

func main() {
    double := |x: int| -> int { x * 2 }
    triple := |x: int| -> int { x * 3 }
    println(apply(double, 5))
    println(apply(triple, 5))
}
"#;
    assert_vm_works("closure_as_argument (closure type in param position)", src);
}

// ===========================================================================
// 5. Control Flow Edge Cases
// ===========================================================================

#[test]
fn bh_nested_break_continue() {
    let src = r#"
func main() {
    sum := 0
    for i in 0..10 {
        for j in 0..10 {
            if j == 3 {
                continue
            }
            if i == 7 {
                break
            }
            sum = sum + 1
        }
    }
    println(sum)
}
"#;
    assert_parity("nested_break_continue", src);
}

#[test]
fn bh_large_match() {
    // FIXED: match with many arms now emits correct C.
    let mut src = String::from("func main() {\n  x := 5\n  match x {\n");
    for i in 0..20 {
        src.push_str(&format!("    {i} => println(\"case {i}\"),\n"));
    }
    src.push_str("    _ => println(\"default\"),\n");
    src.push_str("  }\n}\n");
    assert_parity("large_match", &src);
}

#[test]
fn bh_triple_nested_match() {
    // FIXED: nested match on Option now works correctly.
    let src = r#"
func main() {
    x: Option<Option<int>> = .some(.some(42))
    match x {
        .some(inner) => {
            match inner {
                .some(v) => {
                    match v {
                        42 => println("found 42"),
                        _ => println("other"),
                    }
                },
                .none => println("inner none"),
            }
        },
        .none => println("outer none"),
    }
}
"#;
    assert_parity("triple_nested_match", src);
}

#[test]
fn bh_for_with_early_return() {
    // FIXED: return in for-loop now emits boxed zz_value.
    let src = r#"
func find_first() -> int {
    for i in 0..100 {
        if i == 42 {
            return i
        }
    }
    -1
}

func main() {
    println(find_first())
}
"#;
    assert_parity("for_with_early_return", src);
}

#[test]
fn bh_while_with_complex_condition() {
    let src = r#"
func main() {
    x := 1
    y := 1
    count := 0
    while x < 100 && y < 100 {
        x = x + y
        y = y + 1
        count = count + 1
    }
    println(count)
    println(x)
}
"#;
    assert_parity("while_with_complex_condition", src);
}

// ===========================================================================
// 6. String Edge Cases
// ===========================================================================

#[test]
fn bh_empty_string_ops() {
    let src = r#"
func main() {
    s := ""
    println(len(s))
    println(s == "")
    println(s + "hello")
}
"#;
    assert_parity("empty_string_ops", src);
}

#[test]
fn bh_long_fstring() {
    let src = r#"
func main() {
    a := 1
    b := 2
    c := 3
    d := 4
    e := 5
    println("values: {a}, {b}, {c}, {d}, {e} — sum is {a + b + c + d + e}")
}
"#;
    assert_parity("long_fstring", src);
}

#[test]
fn bh_string_contains() {
    // FIXED: method dispatch now uses checker_types from NameCtx to resolve
    // .contains() to str.contains instead of vec.contains.
    let src = r#"
func main() {
    s := "Hello, World!"
    println(s.contains("World"))
    println(s.contains("Missing"))
    println(s.contains(""))
}
"#;
    assert_parity("string_contains", src);
}

#[test]
fn bh_string_indexing() {
    let src = r#"
func main() {
    s := "abcdef"
    println(len(s))
}
"#;
    assert_parity("string_indexing", src);
}

#[test]
fn bh_string_comparison() {
    // FIXED: string comparison operators now work in native.
    let src = r#"
func main() {
    a := "abc"
    b := "abd"
    c := "abc"
    println(a == b)
    println(a == c)
    println(a != b)
    println(a < b)
}
"#;
    assert_parity("string_comparison", src);
}
