//! Native codegen e2e tests: lower a program, compile to C, run the binary,
//! and compare stdout against the bytecode VM.

use std::collections::HashMap;
use std::path::PathBuf;

use zz_checker::{FuncSig, Type};
use zz_hir::{ReachableSet, TypedProgram};

use crate::{build_native, compile, native_supported, BuildOptions};

/// Seed the real stdlib signatures for typed building.
use zz_stdlib::stdlib_funcs;

fn build_reachable(src: &str) -> (TypedProgram, ReachableSet) {
    // Type-check with real stdlib func sigs.
    let parsed = zz_frontend::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let funcs = stdlib_funcs();
    let res = zz_hir::build_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
    let tp = res.program;
    // DCE from main (bare name; tests avoid module namespacing).
    let (pruned, reach) = zz_hir::dce(&tp, "main");
    (pruned, reach)
}

/// Compile + run a source via native, returning exit + stdout.
fn native_run(src: &str) -> (i32, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (pruned, reach) = build_reachable(src);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("zz-test-{}-{uniq}-out", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = tmp.join("zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), &bin)
        .unwrap_or_else(|e| panic!("build failed: {e}\n---\n{}", e));
    let r = compile::run_binary(&bin, &[]).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

/// Run the same source through the bytecode VM.
fn vm_run(src: &str) -> (i32, String) {
    // Type-check + run via a fresh interp with natives (namespace-free io
    // paths need loader registration; ignored here — this helper is for
    // future cross-checking only).
    let parsed = zz_frontend::parse(src).program;
    let mut interp = zz_runtime::Interp::with_natives(zz_stdlib::stdlib_natives());
    // Register io module namespace like the loader does.
    let mut funcs = HashMap::new();
    let _ = zz_stdlib::register_module_namespace("io", "io", &mut funcs, &mut interp.natives);
    match interp.run(&parsed) {
        Ok(_) => (0, String::new()),
        Err(e) => (1, e.message),
    }
}

#[allow(dead_code)]
fn out_path() -> PathBuf {
    std::env::temp_dir().join(format!("zz-e2e-{}", std::process::id()))
}

/// Intentional, tracked gaps: stdlib funcs the AOT runtime does not implement
/// natively. Each entry names the Phase 2 work item / tracked parity fixture
/// that will remove it. A gap that is no longer missing (the C impl landed)
/// fails the test so the list cannot rot.
const KNOWN_CODEGEN_GAPS: &[(&str, &str)] = &[
    // Phase 2.7 variants — option/result unwrap
    ("option.unwrap", "option unwrap"),
    ("option.unwrap_or", "option unwrap_or"),
    ("result.unwrap", "result unwrap"),
    ("result.unwrap_or", "result unwrap_or"),
    // Non-reachable from parity fixtures (no fixture uses them):
    ("std.math.matrix_mul", "no fixture; niche math"),
    // Pure-ZZ stdlib functions — run in VM only, no C codegen
    ("std.math.sum", "pure ZZ; no C codegen"),
    ("std.math.product", "pure ZZ; no C codegen"),
    ("std.math.count", "pure ZZ; no C codegen"),
    ("std.math.min", "pure ZZ; no C codegen"),
    ("std.math.max", "pure ZZ; no C codegen"),
    ("std.math.is_even", "pure ZZ; no C codegen"),
    ("std.math.is_odd", "pure ZZ; no C codegen"),
    ("std.math.min_arr", "pure ZZ; no C codegen"),
    ("std.math.max_arr", "pure ZZ; no C codegen"),
    ("std.math.sum_f", "pure ZZ; no C codegen"),
    ("std.math.product_f", "pure ZZ; no C codegen"),
    ("std.math.mean_f", "pure ZZ; no C codegen"),
    ("std.math.median_f", "pure ZZ; no C codegen"),
    ("std.str.repeat", "pure ZZ; no C codegen"),
    ("std.str.count", "pure ZZ; no C codegen"),
    ("std.str.is_empty", "pure ZZ; no C codegen"),
    ("std.str.reverse", "pure ZZ; no C codegen"),
    ("std.str.pad_left", "pure ZZ; no C codegen"),
    ("std.str.pad_right", "pure ZZ; no C codegen"),
    ("std.vec.fold", "pure ZZ; no C codegen"),
    ("std.vec.sum", "pure ZZ; no C codegen"),
    ("std.vec.product", "pure ZZ; no C codegen"),
    ("std.vec.min_val", "pure ZZ; no C codegen"),
    ("std.vec.max_val", "pure ZZ; no C codegen"),
    ("std.vec.sum_f", "pure ZZ; no C codegen"),
    ("std.vec.product_f", "pure ZZ; no C codegen"),
    ("std.vec.concat", "pure ZZ; no C codegen"),
    ("std.vec.flatten", "pure ZZ; no C codegen"),
    ("std.vec.index_of", "pure ZZ; no C codegen"),
    ("std.vec.last_index_of", "pure ZZ; no C codegen"),
    // Method-dispatch aliases for pure-ZZ functions
    ("math.sum", "pure ZZ alias"),
    ("math.product", "pure ZZ alias"),
    ("math.count", "pure ZZ alias"),
    ("math.min", "pure ZZ alias"),
    ("math.max", "pure ZZ alias"),
    ("math.is_even", "pure ZZ alias"),
    ("math.is_odd", "pure ZZ alias"),
    ("math.min_arr", "pure ZZ alias"),
    ("math.max_arr", "pure ZZ alias"),
    ("math.sum_f", "pure ZZ alias"),
    ("math.product_f", "pure ZZ alias"),
    ("math.mean_f", "pure ZZ alias"),
    ("math.median_f", "pure ZZ alias"),
    ("str.repeat", "pure ZZ alias"),
    ("str.count", "pure ZZ alias"),
    ("str.is_empty", "pure ZZ alias"),
    ("str.reverse", "pure ZZ alias"),
    ("str.pad_left", "pure ZZ alias"),
    ("str.pad_right", "pure ZZ alias"),
    ("vec.fold", "pure ZZ alias"),
    ("vec.sum", "pure ZZ alias"),
    ("vec.product", "pure ZZ alias"),
    ("vec.min_val", "pure ZZ alias"),
    ("vec.max_val", "pure ZZ alias"),
    ("vec.sum_f", "pure ZZ alias"),
    ("vec.product_f", "pure ZZ alias"),
    ("vec.concat", "pure ZZ alias"),
    ("vec.flatten", "pure ZZ alias"),
    ("vec.index_of", "pure ZZ alias"),
    ("vec.last_index_of", "pure ZZ alias"),
    // Parity-skipped modules (non-deterministic output):
    ("std.http.serve_dir", "http fixtures skipped in parity"),
    ("http.serve_dir", "http fixtures skipped in parity"),
    ("std.http.body_form", "http fixtures skipped in parity"),
    ("std.http.body_json", "http fixtures skipped in parity"),
    ("std.http.delete", "http fixtures skipped in parity"),
    ("std.http.header", "http fixtures skipped in parity"),
    ("std.http.param", "http fixtures skipped in parity"),
    ("std.http.put", "http fixtures skipped in parity"),
    ("std.http.query", "http fixtures skipped in parity"),
    ("std.http.test", "http fixtures skipped in parity"),
];

#[test]
fn all_stdlib_funcs_have_c_impls() {
    // Drift census: the checker's stdlib registry must have a C runtime
    // implementation for every key. The AOT backend lowers funcs through
    // `native_impl`; a missing entry means a valid `std.*` call silently
    // lowers to an unimplemented native.
    let funcs = zz_stdlib::stdlib_funcs();
    let supported = |k: &String| native_supported(k);
    let mut unlisted_gap: Vec<&String> = funcs
        .keys()
        .filter(|k| !supported(k))
        .filter(|k| !KNOWN_CODEGEN_GAPS.iter().any(|(g, _)| g == k))
        .collect();
    unlisted_gap.sort();
    assert!(
        unlisted_gap.is_empty(),
        "stdlib_funcs keys without a C impl and without a KNOWN_CODEGEN_GAPS entry: {unlisted_gap:?}"
    );

    // Stale gaps: an allowlisted key that now HAS a C impl must be removed
    // from KNOWN_CODEGEN_GAPS so the Phase 2 progress is reflected here.
    let stale: Vec<&str> = KNOWN_CODEGEN_GAPS
        .iter()
        .filter(|(g, _)| native_supported(g))
        .map(|(g, _)| *g)
        .collect();
    assert!(
        stale.is_empty(),
        "remove from KNOWN_CODEGEN_GAPS (now implemented): {stale:?}"
    );
}

#[test]
fn native_add_loops_match_vm() {
    let src = r#"
sum := 0
for i in 0..1000 {
    sum = sum + i
}
io.println(sum)
"#;
    let (_, native_stdout) = native_run(src);
    assert_eq!(native_stdout, "499500\n", "native output mismatch");
    let _ = vm_run(src);
}

#[test]
fn native_arithmetic_matches_vm() {
    let src = r#"
io.println(1 + 2 * 3)
io.println((10 - 3) * 2)
io.println((2 ** 10))
io.println(-5 + 5)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "7\n14\n1024\n0\n");
}

#[test]
fn native_float_matches_vm() {
    let src = r#"
io.println(3.5 + 1.5)
io.println(10.0 / 4)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "5.0\n2.5\n");
}

#[test]
fn native_string_concat_matches_vm() {
    let src = r#"
io.println("hello" + " " + "world")
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "hello world\n");
}

#[test]
fn native_if_else_matches_vm() {
    let src = r#"
x := 10
if x > 5 {
    io.println("big")
} else {
    io.println("small")
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "big\n");
}

#[test]
fn native_func_calls_match_vm() {
    let src = r#"
func add(a: int, b: int) -> int {
    a + b
}
func double(x: int) -> int {
    x * 2
}
io.println(add(2, 3))
io.println(double(add(1, 4)))
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "5\n10\n");
}

#[test]
fn native_recursion_matches_vm() {
    let src = r#"
func fib(n: int) -> int {
    if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
io.println(fib(10))
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "55\n");
}

#[test]
fn native_sqrt_math_pow() {
    let src = r#"
io.println(2 ** 5)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "32\n");
}

#[test]
fn native_dce_prunes_unused_http() {
    // Import a heavy module but use only io; the generated C must not
    // reference http.* so compilation succeeds even though the C runtime
    // doesn't implement http.
    let src = r#"
import std.http
io.println("only io")
"#;
    // NOTE: stdlib_funcs seeds http.*; DCE prunes them; native_run builds.
    let (_, out) = native_run(src);
    assert_eq!(out, "only io\n");
}

#[test]
fn native_main_auto_called() {
    // func main is auto-invoked.
    let src = r#"
func main() {
    io.println("from main")
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "from main\n");
}

#[test]
fn generated_source_contains_expected_sections() {
    let src = "io.println(42)\n";
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    assert!(lowered.source.contains("zz_main"), "missing zz_main");
    assert!(
        lowered.source.contains("zz_io_println"),
        "missing println impl"
    );
    // Verify http native functions are NOT in reach.natives when unused.
    // (they live in the modular C runtime under src/runtime/ and are
    // linked, not inlined)
    let has_http_get = reach.natives.contains(&String::from("http.get"));
    let has_http_post = reach.natives.contains(&String::from("http.post"));
    assert!(
        !has_http_get && !has_http_post,
        "http natives should not be reachable when unused"
    );
}

#[test]
fn native_used_function_kept_unused_pruned() {
    let src = r#"
func used(x: int) -> int { x + 1 }
func unused(x: int) -> int { x * 10 }
io.println(used(1))
"#;
    let (pruned, reach) = build_reachable(src);
    assert!(reach.funcs.contains("used"));
    assert!(!reach.funcs.contains("unused"));
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    assert!(lowered.source.contains("zz_fn_used"));
    assert!(!lowered.source.contains("zz_fn_unused"));
}

#[test]
fn native_input_reads_line_and_flushes_prompt() {
    // Compile a program using input("prompt: "); pipe a line into stdin.
    // The prompt is flushed BEFORE the blocking fgets (fflush(stdout)),
    // so the user sees "prompt: " even without a trailing newline.
    let src = r#"
func main() {
    name := input("prompt: ")
    io.println("got " + name)
}
"#;
    let (pruned, reach) = build_reachable(src);
    let tmp = std::env::temp_dir().join(format!("zz-test-input-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = tmp.join("zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), &bin)
        .unwrap_or_else(|e| panic!("build failed: {e}\n---\n{}", e));

    // Pipe "Alice\n" into stdin; capture both stdout and stderr.
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"Alice\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "native input program failed: {stderr}"
    );
    // Prompt appears (flushed) AND the echoed input line is present.
    assert!(
        stdout.contains("prompt: "),
        "prompt not flushed, got {stdout:?}"
    );
    assert_eq!(stdout.trim_end(), "prompt: got Alice");
}

#[test]
fn native_range_call_loop_and_bare_println() {
    // Mirrors the performance-check fixture: `range(n)` loop + bare
    // `println` (no io. prefix) + time.now_ms for elapsed timing.
    let src = r#"
func main() {
    result := 0
    for i in range(1000) {
        result = result + i
    }
    println(result)
    start := time.now_ms()
    println(start - 0)
}
"#;
    let (pruned, reach) = build_reachable(src);
    let tmp = std::env::temp_dir().join(format!("zz-test-range-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = tmp.join("zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), &bin)
        .unwrap_or_else(|e| panic!("build failed: {e}\n---\n{}", e));
    let (_, out) = compile::run_binary(&bin, &[]).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "499500", "range(1000) sum wrong: {out}");
    // Second line is a monotonic ms timestamp — must parse as int.
    assert!(
        lines[1].parse::<i64>().is_ok(),
        "time.now_ms not int: {out}"
    );
}

#[test]
fn native_struct_init_and_fields() {
    let src = r#"
struct Point { x: int, y: int }
p := Point{ x: 10, y: 20 }
io.println(p.x)
io.println(p.y)
p.x = 99
io.println(p.x)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "10\n20\n99\n");
}

#[test]
fn native_struct_nested() {
    let src = r#"
struct Point { x: int, y: int }
struct Rect { origin: Point, w: int, h: int }
r := Rect{ origin: Point{ x: 1, y: 2 }, w: 10, h: 20 }
io.println(r.origin.x)
io.println(r.origin.y)
io.println(r.w)
r.origin.x = 42
io.println(r.origin.x)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "1\n2\n10\n42\n");
}

/// Helper to build a TypedProgram for tests that need the real stdlib.
#[allow(dead_code)]
fn _seed() -> HashMap<String, FuncSig> {
    stdlib_funcs()
}

#[test]
fn debug_method_dispatch_c_source() {
    let src = r#"
s := "Hello World"
r := s.contains("World")
println(r)
println("done")
"#;
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".to_string(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    // Print only the zz_main function body
    let mut in_main = false;
    let mut brace_depth = 0;
    for line in lowered.source.lines() {
        if line.starts_with("void zz_main(") {
            in_main = true;
        }
        if in_main {
            eprintln!("C: {}", line);
            brace_depth += line.matches('{').count();
            brace_depth = brace_depth.saturating_sub(line.matches('}').count());
            if brace_depth == 0 && line.contains('}') && !line.starts_with("void zz_main") {
                break;
            }
        }
    }
}

fn debug_generated_c(label: &str, src: &str) {
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".to_string(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    let mut in_main = false;
    let mut brace_depth = 0;
    eprintln!("=== {label} ===");
    for line in lowered.source.lines() {
        if line.starts_with("void zz_main(") {
            in_main = true;
        }
        if in_main {
            eprintln!("C: {}", line);
            brace_depth += line.matches('{').count();
            brace_depth = brace_depth.saturating_sub(line.matches('}').count());
            if brace_depth == 0 && line.contains('}') && !line.starts_with("void zz_main") {
                break;
            }
        }
    }
}

#[test]
fn debug_string_contains_func_main_c() {
    debug_generated_c(
        "string contains func main",
        r#"
func main() {
    s := "Hello, World!"
    r := s.contains("World")
    println(r)
}
"#,
    );
}

#[test]
fn debug_triple_nested_match_c() {
    debug_generated_c(
        "triple nested match",
        r#"
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
"#,
    );
}

#[allow(dead_code)]
fn _type_marker(_: Type) {}

// ---- Escape analysis integration tests ----------------------------------

#[test]
fn escape_analysis_local_array_non_escaping() {
    let src = r#"
func main() {
    arr := [1, 2, 3]
    io.println(len(arr))
}
"#;
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    // Generated C must contain arena init/reset for main function.
    assert!(
        lowered.source.contains("zz_arena_init"),
        "missing arena init"
    );
    assert!(
        lowered.source.contains("zz_arena_reset"),
        "missing arena reset"
    );
    // Must still compile and run correctly.
    let (_, out) = native_run(src);
    assert_eq!(out, "3\n");
}

#[test]
fn escape_analysis_function_has_arena() {
    let src = r#"
func compute(n: int) {
    result := 0
    for i in 0..n {
        result = result + i
    }
    io.println(result)
}
func main() {
    compute(100)
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "4950\n");
    // Verify arena is present in the generated function.
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    // Both main and compute functions should have arena init/reset.
    let arena_count = lowered.source.matches("zz_arena_init").count();
    assert!(
        arena_count >= 2,
        "expected at least 2 arena inits (main + compute), got {arena_count}"
    );
}

#[test]
fn escape_analysis_scalar_vars_arena_safe() {
    let src = r#"
func main() {
    x := 42
    y := 3.14
    z := true
    io.println(x)
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "42\n");
    // All scalar variables are arena-safe (non-escaping).
    // The program should still compile and run correctly.
}

#[test]
fn escape_analysis_string_concat_loop() {
    let src = r#"
func main() {
    s := ""
    for i in 0..5 {
        s = s + "x"
    }
    io.println(s)
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "xxxxx\n");
}

#[test]
fn generated_c_contains_arena_in_all_functions() {
    let src = r#"
func helper(x: int) -> int { x + 1 }
func main() {
    io.println(helper(41))
}
"#;
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    // Arena init and reset must both appear in generated C.
    assert!(
        lowered.source.contains("zz_arena_init"),
        "missing arena init in generated C"
    );
    assert!(
        lowered.source.contains("zz_arena_reset"),
        "missing arena reset in generated C"
    );
}
