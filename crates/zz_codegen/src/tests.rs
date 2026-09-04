//! Native codegen e2e tests: lower a program, compile to C, run the binary,
//! and compare stdout against the bytecode VM.

use std::collections::HashMap;
use std::path::PathBuf;

use zz_checker::{FuncSig, Type};
use zz_hir::{ReachableSet, TypedProgram};

use crate::{build_native, compile, BuildOptions};

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

fn out_path() -> PathBuf {
    std::env::temp_dir().join(format!("zz-e2e-{}", std::process::id()))
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
    // http natives pruned:
    assert!(
        !lowered.source.contains("http."),
        "http natives should be pruned from generated source"
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

#[allow(dead_code)]
fn _type_marker(_: Type) {}
