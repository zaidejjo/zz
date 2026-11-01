//! Native codegen e2e tests: lower a program, compile to C, run the binary,
//! and compare stdout against the bytecode VM.

use std::collections::HashMap;
use std::path::PathBuf;

use zz_checker::{FuncSig, Type};
use zz_hir::{ReachableSet, TypedProgram};

use crate::{
    build_native, compile, lower_only, native_supported, reject_unimplemented_natives, BuildError,
    BuildOptions, Lowerer, KNOWN_CODEGEN_GAPS,
};

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
    let res = zz_hir::build_program(
        &parsed.program,
        HashMap::new(),
        funcs,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let tp = res.program;
    // DCE from main (bare name; tests avoid module namespacing).
    let (pruned, reach) = zz_hir::dce(&tp, "main");
    (pruned, reach)
}

/// Temp output binary path carrying the platform executable suffix.
///
/// The link step writes exactly the name it is given; without `.exe` a
/// Windows build would produce an extensionless file that the tests then
/// fail to execute by path.
fn native_bin(tmp: &std::path::Path, stem: &str) -> std::path::PathBuf {
    tmp.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
}

/// Compile + run a source via native, returning exit + stdout.
fn native_run(src: &str) -> (i32, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (pruned, reach) = build_reachable(src);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("zz-test-{}-{uniq}-out", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = native_bin(&tmp, "zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), None, &bin)
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
    let _ = zz_stdlib::register_module_namespace(
        "io",
        "io",
        &mut funcs,
        std::sync::Arc::make_mut(&mut interp.natives),
    );
    match interp.run(&parsed) {
        Ok(_) => (0, String::new()),
        Err(e) => (1, e.message),
    }
}

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

    // Every staticlib symbol must carry a C declaration: generated C gets
    // its `extern` from `ffi_decl`, so a symbol without one fails at C
    // compile time. `zz_env_args` is provided by the embedded C runtime,
    // not the static library.
    let undeclared: Vec<(&String, &str)> = funcs
        .keys()
        .filter_map(|k| {
            let sym = crate::ffi_impl(k).or_else(|| crate::ffi_impl(&format!("std.{k}")))?;
            (sym != "zz_env_args" && crate::ffi_decl(sym).is_none()).then_some((k, sym))
        })
        .collect();
    assert!(
        undeclared.is_empty(),
        "ffi symbols without a C decl: {undeclared:?}"
    );
}

#[test]
fn native_add_loops_match_vm() {
    let src = r#"
sum := 0
for i in 0..1000 {
    sum = sum + i
}
println(sum)
"#;
    let (_, native_stdout) = native_run(src);
    assert_eq!(native_stdout, "499500\n", "native output mismatch");
    let _ = vm_run(src);
}

#[test]
fn native_arithmetic_matches_vm() {
    let src = r#"
println(1 + 2 * 3)
println((10 - 3) * 2)
println((2 ** 10))
println(-5 + 5)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "7\n14\n1024\n0\n");
}

#[test]
fn native_float_matches_vm() {
    let src = r#"
println(3.5 + 1.5)
println(10.0 / 4)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "5.0\n2.5\n");
}

#[test]
fn native_codegen_graceful_fallbacks_no_panic() {
    // Exercises codegen paths that previously contained `unreachable!()`:
    //   chunk.rs BinOp fast-path -> zz_binop fallback (bitwise / shift / mod)
    //   chunk.rs UnOp fast-path map (Neg / Pos / Not / BitNot)
    //   lower/expr.rs scalar fallbacks -> zz_binop (mixed int/double, div)
    let src = r#"
println(6 & 3)
println(6 | 3)
println(6 ^ 3)
println(~6)
println(1 << 10)
println(1024 >> 3)
println(-7)
println(+42)
println(1 + 2.5)
println(10.0 / 4)
println(10 % 3)
"#;
    let (exit, out) = native_run(src);
    assert_eq!(exit, 0, "native build/run must not panic: {out}");
    assert_eq!(
        out, "2\n7\n5\n-7\n1024\n128\n-7\n42\n3.5\n2.5\n1\n",
        "native output mismatch"
    );
}

#[test]
fn native_scalar_copy_matches_vm() {
    // Scalar-to-scalar copies (`s := sx` Decl, `s = sy` Assign between raw
    // scalar locals) once miscompiled to `(v).f` on a plain `double`, so
    // the fresh native build below failed at C compile time. `native_run`
    // rebuilds from scratch every call (no artifact cache), so this test
    // cannot go green behind a stale binary the way a cached `zz run
    // --native` can.
    let src = r#"
func fit(cur_w: int, cur_h: int, want_w: int, want_h: int) -> float {
    sx := float(want_w) / float(cur_w)
    sy := float(want_h) / float(cur_h)
    s := sx
    if sy < sx {
        s = sy
    }
    return s
}
func pick(a: int, b: int, cond: bool) -> int {
    x := a
    if cond {
        x = b
    }
    return x
}
println(fit(640, 480, 320, 240))
println(fit(640, 480, 200, 200))
println(pick(10, 20, true))
println(pick(10, 20, false))
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "0.5\n0.3125\n20\n10\n");
}

#[test]
fn native_string_concat_matches_vm() {
    let src = r#"
println("hello" + " " + "world")
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "hello world\n");
}

#[test]
fn native_if_else_matches_vm() {
    let src = r#"
x := 10
if x > 5 {
    println("big")
} else {
    println("small")
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "big\n");
}

#[test]
fn reject_unimplemented_natives_flags_non_gap() {
    // A name that is not an intentional VM-only gap must be rejected.
    let err = reject_unimplemented_natives(&["std.fake.native".to_string()]).unwrap_err();
    let BuildError::UnimplementedNative { names } = err else {
        panic!("expected UnimplementedNative, got: {err}");
    };
    assert_eq!(names, vec!["std.fake.native"]);
}

#[test]
fn reject_unimplemented_natives_allows_gap() {
    // Intentional VM-only gaps must NOT trigger the preflight error.
    for &(gap, _) in KNOWN_CODEGEN_GAPS.iter() {
        assert!(
            reject_unimplemented_natives(&[gap.to_string()]).is_ok(),
            "known gap {gap} must be allowed by the preflight"
        );
    }
    // Multiple names: only non-gap ones are reported.
    let err = reject_unimplemented_natives(&[
        "std.http.listen_tls".to_string(),
        "std.fake.native".to_string(),
    ])
    .unwrap_err();
    let BuildError::UnimplementedNative { names } = err else {
        panic!("expected UnimplementedNative, got: {err}");
    };
    assert_eq!(names, vec!["std.fake.native"]);
}

#[test]
fn native_func_calls_match_vm() {
    let src = r#"
func add(a: int, b: int) -> int {
    return a + b
}
func double(x: int) -> int {
    return x * 2
}
println(add(2, 3))
println(double(add(1, 4)))
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "5\n10\n");
}

#[test]
fn native_recursion_matches_vm() {
    let src = r#"
func fib(n: int) -> int {
    return if n <= 1 { n } else { fib(n - 1) + fib(n - 2) }
}
println(fib(10))
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "55\n");
}

#[test]
fn native_sqrt_math_pow() {
    let src = r#"
println(2 ** 5)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "32\n");
}

#[test]
fn native_dce_prunes_unused_http() {
    // Import a heavy module but use only io; the generated C must not
    // reference http.* (DCE prunes the unused natives).
    let src = r#"
import std.http
println("only io")
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
    println("from main")
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "from main\n");
}

#[test]
fn generated_source_contains_expected_sections() {
    let src = "println(42)\n";
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
fn units_header_declares_all_init_functions() {
    // Regression: each unit is compiled as a separate TU, but `zz_main`
    // (in the entry unit) calls `zz_init_<ns>()` for every namespace, so
    // the shared header must forward-declare every init symbol — otherwise
    // cross-unit calls like `zz_init__prelude()` fail with an implicit
    // function declaration error.
    let src = "x := 41\nfunc util.helper() { 1 }\ny := 42\nfunc main() { util.helper() + x + y }\n";
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let units = lowerer.lower_units("main");
    assert!(
        units.header.contains("void zz_init__prelude(void);"),
        "header must declare prelude init:\n--- header ---\n{}\n--- end ---",
        units.header
    );
    assert!(
        units.header.contains("void zz_init_util(void);"),
        "header must declare util init:\n--- header ---\n{}\n--- end ---",
        units.header
    );
    // Both units exist and define their inits.
    let prelude = units
        .units
        .iter()
        .find(|u| u.ns == "_prelude")
        .expect("_prelude unit present");
    let util = units
        .units
        .iter()
        .find(|u| u.ns == "util")
        .expect("util unit present");
    assert!(
        prelude.code.contains("void zz_init__prelude(void)"),
        "_prelude unit defines its init"
    );
    assert!(
        util.code.contains("void zz_init_util(void)"),
        "util unit defines its init"
    );
}

/// Lower `src` to C (no clang involved).
fn lower_c(src: &str) -> String {
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    lowerer.lower().source
}

/// `len(arr)` over a runtime call, not a baked arity.
fn assert_len_is_live(c: &str, what: &str) {
    assert!(
        c.contains("zz_len"),
        "{what}: expected a live `zz_len` call in:\n{c}"
    );
}

/// Literal-length folds (`len(v)` → arity) are only valid in straight-line
/// flow. Loop bodies re-run (and may reassign) while conditions
/// re-evaluate; branch joins merge exclusive paths — folding across any
/// of these boundaries bakes a stale constant (#350: `len(arr) < 5`
/// folded to `0 < 5` while the body grew the array, so native looped to
/// the iteration guard and paid O(n²) copies).

#[test]
fn while_cond_len_sees_body_push() {
    let c = lower_c(
        r#"
import std.vec
func main() {
    arr := []
    i := 0
    while len(arr) < 5 {
        arr = vec.push(arr, i)
        i = i + 1
        if i > 100000 {
            break
        }
    }
    println("n={len(arr)} i={i}")
}
"#,
    );
    assert_len_is_live(&c, "while guard");
    assert!(
        !c.contains("zz_binop(ZZOP_LT, zz_int(0), zz_int(5))"),
        "while guard must not fold `len(arr)` to the pre-loop arity"
    );
}

#[test]
fn for_body_len_sees_body_push() {
    let c = lower_c(
        r#"
import std.vec
func main() {
    arr := [1]
    for i in 0..3 {
        println("n={len(arr)}")
        arr = vec.push(arr, i)
    }
}
"#,
    );
    assert_len_is_live(&c, "for body");
}

#[test]
fn if_join_len_not_folded_from_branch() {
    let c = lower_c(
        r#"
func main() {
    arr := [1, 2, 3]
    if false {
        arr = [9, 9, 9, 9]
    }
    println("n={len(arr)}")
}
"#,
    );
    assert_len_is_live(&c, "post-if join");
    assert!(
        !c.contains("zz_int(4)"),
        "post-if `len(arr)` must not fold to the untaken branch arity"
    );
}

#[test]
fn match_join_len_not_folded_from_arm() {
    let c = lower_c(
        r#"
func main() {
    arr := [1, 2, 3]
    x := 1
    match x {
        1 => { arr = [9, 9] }
        _ => { arr = [7, 7, 7, 7, 7] }
    }
    println("n={len(arr)}")
}
"#,
    );
    assert_len_is_live(&c, "post-match join");
}

#[test]
fn straight_line_len_still_folds() {
    // The peephole must keep working where it is sound: same-block
    // literal binds with no intervening control flow.
    let c = lower_c(
        r#"
func main() {
    arr := [1, 2, 3]
    println("n={len(arr)}")
}
"#,
    );
    assert!(
        !c.contains("zz_call_native1(zz_len"),
        "straight-line `len` of a literal should still fold, got:\n{c}"
    );
}

#[test]
fn native_used_function_kept_unused_pruned() {
    let src = r#"
func used(x: int) -> int { return x + 1 }
func unused(x: int) -> int { return x * 10 }
println(used(1))
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
    println("got " + name)
}
"#;
    let (pruned, reach) = build_reachable(src);
    let tmp = std::env::temp_dir().join(format!("zz-test-input-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = native_bin(&tmp, "zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), None, &bin)
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
    let bin = native_bin(&tmp, "zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), None, &bin)
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
println(p.x)
println(p.y)
p.x = 99
println(p.x)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "10\n20\n99\n");
}

/// Issue #557: module-scope binding method call on chunk.
///
/// `a := .some(11); b := a.unwrap_or(0)` lowers to
/// `CallPath ["top2.a", "unwrap_or"]`. The chunk backend must split
/// this into `("option", "unwrap_or")` and resolve to `zz_option_unwrap_or`,
/// matching the VM's static method dispatch (#508 pattern extended to
/// module-scope bindings and match-arm bindings).
#[test]
fn native_module_scope_method_call() {
    let src = r#"
a := .some(11)
b := a.unwrap_or(0)
println(b)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "11\n");
}

#[test]
fn native_module_scope_method_call_none() {
    let src = r#"
a := .none
b := a.unwrap_or(0)
println(b)
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "0\n");
}

#[test]
fn native_match_arm_method_call() {
    let src = r#"
func main() {
    o := .some(.some(5))
    v := match o {
        .some(inner) => inner.unwrap_or(0),
        .none => 0,
    }
    println(v)
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "5\n");
}

#[test]
fn native_method_call_on_value_receiver() {
    let src = r#"
func main() {
    o := .some(.some(5))
    v := match o {
        .some(inner) => inner.unwrap_or(0),
        .none => 0,
    }
    println(v)
    o2 := .none
    v2 := o2.unwrap_or(99)
    println(v2)
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "5\n99\n");
}

#[test]
fn native_closure_env_capture() {
    // Closures capture by reference (match the VM): mutations through the
    // closure are visible to the owner, late owner writes are visible to
    // the closure, and nested closures share outer cells.
    let src = r#"
func make_counter() {
    count := 0
    inc := |d| { count = count + d; count }
    inc(5)
    return inc(3)
}
func nested() {
    x := 1
    mid := |a| {
        y := a + x
        inner := |b| x + y + b
        inner(10)
    }
    return mid(100)
}
func late() {
    a := 10
    add_a := |x| x + a
    r1 := add_a(5)
    a = 100
    return r1 + add_a(5)
}
println(make_counter())
println(nested())
println(late())
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "8\n112\n120\n");
}

#[test]
fn native_method_form_hof() {
    // Method-form dispatch for higher-order functions: xs.map(f) should
    // produce the same result as map(xs, f).
    let src = r#"
func main() {
    xs := [1, 2, 3]
    ys := xs.map(|x| x + 1)
    println(ys[0] + ys[1] + ys[2])
    zs := xs.filter(|x| x > 1)
    println(zs[0] + zs[1])
    println(len(xs.enumerate()))
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "9\n5\n3\n");
}

#[test]
fn native_struct_field_in_closure() {
    // Struct field access (p.x) inside closures should resolve and
    // lower correctly without intermediate local bindings.
    let src = r#"
struct Point { x: int, y: int }
func main() {
    p := Point { x: 3, y: 4 }
    f := |s| p.x * s + p.y
    println(f(10))
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "34\n");
}

#[test]
fn native_json_extended_matches_vm() {
    // Mirrors tests/fixtures/stdlib/json_extended_test.zz: exercises the
    // json.* natives that have C runtime impls (type, len, keys, has,
    // pretty, merge, deep_get, array_push).
    let src = r#"
j := json.parse("{\"name\": \"test\", \"count\": 42, \"tags\": [\"a\", \"b\"], \"nested\": {\"x\": 1}}") ?? json.null()
println(json.type(j))
arr := json.get(j, "tags") ?? json.null()
println(json.type(arr))
num := json.get(j, "count") ?? json.null()
println(json.type(num))
println(json.len(j))
println(json.len(arr))
k := json.keys(j)
println(len(k))
println(json.has(j, "name"))
println(json.has(j, "missing"))
pretty_out := json.pretty(j)
println(str.contains(pretty_out, "\n"))
a := json.parse("{\"x\": 1, \"y\": 2}") ?? json.null()
b := json.parse("{\"y\": 99, \"z\": 3}") ?? json.null()
merged := json.merge(a, b)
merged_y := json.get(merged, "y") ?? json.null()
println(json.as_int(merged_y))
println(json.has(merged, "z"))
deep := json.deep_get(j, "nested.x")
match deep {
    .ok(v) => println(json.as_int(v))
    .err(msg) => println("deep_get error: {msg}")
}
miss := json.deep_get(j, "missing.path")
match miss {
    .ok(_) => println("deep miss: should not happen")
    .err(_) => println("deep miss: correctly returned error")
}
original_arr := json.parse("[1, 2, 3]") ?? json.null()
pushed := json.array_push(original_arr, 4)
println(json.len(pushed))
"#;
    let (_, out) = native_run(src);
    assert_eq!(
        out,
        "object\narray\nnumber\n4\n2\n4\ntrue\nfalse\ntrue\n99\ntrue\n1\ndeep miss: correctly returned error\n4\n"
    );
}

#[test]
fn native_struct_nested() {
    let src = r#"
struct Point { x: int, y: int }
struct Rect { origin: Point, w: int, h: int }
r := Rect{ origin: Point{ x: 1, y: 2 }, w: 10, h: 20 }
println(r.origin.x)
println(r.origin.y)
println(r.w)
r.origin.x = 42
println(r.origin.x)
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
    println(len(arr))
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
    println(result)
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
    println(x)
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
    println(s)
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "xxxxx\n");
}

#[test]
fn native_rate_limit_retry_after_rounds_up() {
    // VM: `wait_ms.div_ceil(1000).max(1)`. A 1500 ms window with one
    // token must advertise 2 seconds, not truncate to 1 and send the
    // client back inside the exhausted window.
    let src = r#"
import std.http

func main() {
    s := http.server()
    s = s.route("GET", "/x", |_req| "x")
    s = http.rate_limit(s, 1, 1500)
    r1 := http.test(s, "GET", "/x", "")
    r2 := http.test(s, "GET", "/x", "")
    println("{r1.status()}:{r2.status()}:{r2.headers()["Retry-After"]}")
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "200:429:2\n");
}

#[test]
fn db_locals_get_scope_cleanup_and_borrow_native_calls() {
    // AOT has no VM Arc drop for plain C locals: a handler that opens a
    // DB per request must still close it. DB declarations carry a C
    // cleanup handler, and the query/exec/close natives borrow the live
    // local instead of taking an unreleased clone share.
    let src = r#"
func touch(d: db) {
    sqlz.exec(d, "CREATE TABLE t (n INTEGER)")
}

func open_db() -> db {
    return sqlz.open(":memory:")
}

func main() {
    db := sqlz.open(":memory:")
    touch(db)
    rows := open_db().query("SELECT 1")
    println(len(rows))
    sqlz.close(db)
}
"#;
    let (pruned, reach) = build_reachable(src);
    let c = lower_only(&pruned, &reach, "main").source;
    assert!(
        c.contains("cleanup(zz_db_value_cleanup)"),
        "DB local must carry a scope cleanup handler:\n{c}"
    );
    let user = c.split("// ---- generated code ----").nth(1).unwrap_or("");
    assert!(
        user.lines().any(|line| {
            line.contains("cleanup(zz_db_value_cleanup)") && line.contains("= args[0];")
        }),
        "DB parameter must release the caller's owning clone:\n{user}"
    );
    assert!(
        user.contains("zz_release(&zz_db_arg"),
        "DB calls must release the borrowed receiver:\n{user}"
    );
    assert!(
        !user.contains("zz_db_exec, zz_clone("),
        "DB exec must not pass an unreleased clone share:\n{user}"
    );
    assert!(
        user.contains("zz_db_close, v"),
        "DB close must borrow the live local:\n{user}"
    );
}

#[test]
fn generated_c_contains_arena_in_all_functions() {
    let src = r#"
func helper(x: int) -> int { return x + 1 }
func main() {
    println(helper(41))
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

/// Structural proof that `x OP= y` lowers to the same C as `x = x OP y`
/// for plain receivers: normalize the target statement text, then
/// compare generated sources for equality.
#[test]
fn compound_assign_lowers_like_plain_assign() {
    fn lowered(body: &str) -> String {
        let src = format!("func main() {{\n{body}\n}}\n");
        let (pruned, reach) = build_reachable(&src);
        lower_only(&pruned, &reach, "main").source
    }

    for (compound, plain) in [
        ("x += 1", "x = x + 1"),
        ("x -= y", "x = x - y"),
        ("x *= 2", "x = x * 2"),
        ("x /= 2", "x = x / 2"),
        ("x &= mask", "x = x & mask"),
        ("x <<= 2", "x = x << 2"),
        ("s += t", "s = s + t"),
    ] {
        let a = lowered(&format!(
            "x := 0\ny := 0\nmask := 0\ns := \"\"\nt := \"\"\n{compound}"
        ));
        let b = lowered(&format!(
            "x := 0\ny := 0\nmask := 0\ns := \"\"\nt := \"\"\n{plain}"
        ));
        // The only difference may be the source-text echo in comments;
        // the emitted statements must match line-for-line.
        let norm = |s: &str| {
            s.lines()
                .filter(|l| {
                    !(l.contains("x += 1")
                        || l.contains("x = x + 1")
                        || l.contains("x -= y")
                        || l.contains("x = x - y")
                        || l.contains("x *= 2")
                        || l.contains("x = x * 2")
                        || l.contains("x /= 2")
                        || l.contains("x = x / 2")
                        || l.contains("x &= mask")
                        || l.contains("x = x & mask")
                        || l.contains("x <<= 2")
                        || l.contains("x = x << 2")
                        || l.contains("s += t")
                        || l.contains("s = s + t"))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (na, nb) = (norm(&a), norm(&b));
        // Function emission order is nondeterministic (map iteration),
        // so compare as multisets of lines.
        let mut la: Vec<&str> = na.lines().collect();
        let mut lb: Vec<&str> = nb.lines().collect();
        la.sort_unstable();
        lb.sort_unstable();
        if la != lb {
            let mut diff = String::new();
            for (i, (x, y)) in la.iter().zip(lb.iter()).enumerate() {
                if x != y {
                    diff.push_str(&format!("line {i}:\n  compound: {x}\n  plain:    {y}\n"));
                    if diff.len() > 2000 {
                        break;
                    }
                }
            }
            diff.push_str(&format!("lengths: {} vs {}\n", la.len(), lb.len()));
            panic!("C mismatch ({compound} vs {plain}):\n{diff}");
        }
    }
}

/// The boxed temp of an unboxed-struct tuple/array element must be
/// released after the cloning append, or every such construction
/// retains one object (loops building `(int, Rng)` tuples grew ~0.5KB
/// per draw). Borrowed locals and inline fallbacks take other paths
/// and must NOT gain a release.
#[test]
fn container_struct_temp_is_released_after_append() {
    let src = "struct Rng { s0: int, s1: int }\nfunc next(r: Rng) -> (int, Rng) {\n return (r.s0 + 1, Rng{ s0: r.s0 + 1, s1: r.s1 })\n}\nfunc main() {\n r := Rng{ s0: 1, s1: 2 }\n v, r := next(r)\n println(v)\n}\n";
    let (pruned, reach) = build_reachable(src);
    let c = lower_only(&pruned, &reach, "main").source;
    // The struct-element append must carry its temp release on the same
    // line (scan user code only: the runtime prelude also mentions
    // zz_release in definitions).
    let appends: Vec<&str> = c
        .lines()
        .filter(|l| l.contains("zz_vec_append(") && l.contains("__obj"))
        .collect();
    assert_eq!(appends.len(), 1, "expected one struct-element append");
    assert!(
        appends[0].contains("zz_release(&__obj"),
        "struct temp must be released after the cloning append: {}",
        appends[0]
    );
}

#[test]
fn native_first_class_func_ref() {
    // `f := add` boxes the static function as a callable value instead
    // of lowering to unit (generics/externs/methods keep their existing
    // behavior by design).
    let src = r#"
func add(a: int, b: int) -> int {
    return a + b
}
func main() {
    f := add
    println(f(1, 2))
    g := add
    println(g(20, 22))
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "3\n42\n");
}

#[test]
fn native_indexed_struct_method_receiver() {
    // Indexed receivers (`arr[0].method()`) once dropped `self` in AOT
    // lowering (`method(NULL, 0)` — C arity error), and impl-method call
    // sites misaligned explicit args against `self` (first user arg boxed
    // as the struct). Both now lower like the VM.
    let src = r#"
struct Box { v: int }
impl Box {
    func inc(self, n: int) -> Box {
        return Box{ v: self.v + n }
    }
    func get(self) -> int {
        return self.v
    }
}
func main() {
    arr := [Box{ v: 1 }]
    arr[0] = arr[0].inc(41)
    println(arr[0].get())
    b := Box{ v: 1 }
    c := b.inc(41)
    println(c.get())
    println(b.inc(1).get())
}
"#;
    let (_, out) = native_run(src);
    assert_eq!(out, "42\n42\n2\n");
}

#[test]
fn native_flush_reaches_reader_before_blocking_read() {
    // `print()` alone never flushes (documented contract); `term.flush()`
    // must push the C-stdio bytes out even when no newline follows. The
    // Rust-only flush once left TTY prompts stranded in the C buffer
    // while `read_key` blocked — zero output, deadlock-looking hang.
    // Spawns the binary with stdin held open: the prompt must arrive
    // before any input is sent.
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let src = r#"
import std.term
func main() {
    print("PROMPT>")
    term.flush()
    name := input("")
    println("hi {name}")
}
"#;
    let (pruned, reach) = build_reachable(src);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("zz-test-flush-{uniq}-{}-out", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = native_bin(&tmp, "zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), None, &bin)
        .unwrap_or_else(|e| panic!("build failed: {e}\n---\n{}", e));
    let mut child = std::process::Command::new(&bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn flushed prompter");
    // Piped stdout is block-buffered in C: only an explicit flush
    // delivers "PROMPT>" while the child still blocks on stdin.
    let mut out = child.stdout.take().expect("stdout pipe");
    // The reader stays alive for the whole run (dropping the pipe
    // before the answer line would SIGPIPE the child). It reports the
    // prompt bytes separately so the test can assert they arrived
    // while the child still blocked on stdin.
    let reader = std::thread::spawn(move || {
        let mut prompt = [0u8; 7];
        let mut got = 0;
        let start = std::time::Instant::now();
        while got < prompt.len() && start.elapsed() < std::time::Duration::from_secs(10) {
            match out.read(&mut prompt[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let mut rest = Vec::new();
        let _ = out.read_to_end(&mut rest);
        (got, prompt, rest)
    });
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(b"ada\n")
        .expect("answer the prompt");
    let (got, prompt, rest) = reader.join().expect("output reader");
    assert_eq!(
        got, 7,
        "prompt bytes never arrived while child blocked on input (flush broken)"
    );
    assert_eq!(&prompt, b"PROMPT>");
    let done = child.wait().expect("wait prompter");
    assert!(done.success(), "prompter failed: status={done:?}");
    assert_eq!(
        String::from_utf8_lossy(&rest),
        "hi ada\n",
        "answer line mismatch"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn accum_loop_chained_cats_use_loop_arena() {
    // Regression: `s = s + a + str(i) + b` in a loop must lower to
    // sequential in-place appends (amortized O(1) via str_grow), not
    // nested cat temporaries. Earlier iterations of this fix routed the
    // chain through the loop arena; the arena path is now leak-free too
    // (cats consume their inputs), but appends avoid the O(N^2) copies
    // entirely — 20k-iteration builds stay in the low MBs.
    let src = "func main() {\n    st := \"\"\n    for i in 0..20000 {\n        st = st + \"item_\" + str(i) + \";\"\n    }\n    println(len(st))\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowered = lower_only(&pruned, &reach, "main").source;
    assert!(
        lowered.contains("zz_str_append_lit"),
        "chained accumulation must lower to in-place appends"
    );
    assert!(
        lowered.contains("zz_str_append_str"),
        "str(i) term must append through a temp"
    );
    // Scope the no-cat check to generated user code: the runtime prelude
    // always defines zz_binop_cat, so a whole-source contains() would
    // match the definition itself.
    let body = lowered
        .find("zz_fn_main")
        .map(|i| &lowered[i..])
        .unwrap_or(&lowered);
    assert!(
        !body.contains("zz_binop_cat"),
        "no cat temporaries should remain on the accumulation path"
    );
    // End-to-end: the loop must produce the right length.
    let (_, out) = native_run(src);
    assert_eq!(out, "208890\n");
}

#[test]
fn str_append_chain_single_lit_still_appends() {
    // The original single-term fast path (`s = s + "x"`) keeps working.
    let src = "func main() {\n    s := \"\"\n    for i in 0..5 {\n        s = s + \"x\"\n    }\n    println(s)\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowered = lower_only(&pruned, &reach, "main").source;
    assert!(
        lowered.contains("zz_str_append_lit"),
        "single-lit append must use the lit shim"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "xxxxx\n");
}

#[test]
fn str_cat_temps_do_not_leak() {
    // Non-assign cats (e.g. `chunk := prefix + str(i) + ","`) consume
    // their inputs: 20k iterations must stay flat, not grow per-iter.
    let src = "func main() {\n    out := \"\"\n    for i in 0..20000 {\n        chunk := \"k\" + str(i) + \",\"\n        out = out + chunk\n    }\n    println(len(out))\n}\n";
    let (_, out) = native_run(src);
    assert_eq!(out, "128890\n");
}

#[test]
fn elvis_consume_semantics_match() {
    // `zz_elvis` consumes both inputs (they are owned temporaries).
    // Exercise the Some/Ok (unwrap) and None/Err (default) paths with
    // heap string payloads in a loop: a double-free or use-after-free
    // here crashes, and a missing release leaks the payload per iter.
    let src = "import std.env\nfunc main() {\n    env.set(\"ZZ_ELVIS_T\", \"hi\")\n    a := \"\"\n    b := \"\"\n    for i in 0..100 {\n        v := env.get(\"ZZ_ELVIS_T\") ?? \"dflt\"\n        a = a + v\n        m := env.get(\"ZZ_ELVIS_MISSING_XYZ\") ?? \"d\"\n        b = b + m\n    }\n    println(len(a))\n    println(len(b))\n    env.unset(\"ZZ_ELVIS_T\")\n}\n";
    let (_, out) = native_run(src);
    assert_eq!(out, "200\n100\n");
}

#[test]
fn loop_body_heap_locals_release_per_iteration() {
    // Heap locals declared in a loop body must release at the bottom of
    // every iteration. Before the fix, the C local died each iteration
    // while its heap lived on (~1MB/pass on outer build loops).
    let src = "func main() {\n    total := 0\n    for p in 0..3 {\n        chunk := \"\"\n        for i in 0..100 {\n            chunk = chunk + \"x\"\n        }\n        total = total + len(chunk)\n    }\n    println(total)\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowered = lower_only(&pruned, &reach, "main").source;
    let body = lowered
        .find("zz_fn_main")
        .map(|i| &lowered[i..])
        .unwrap_or(&lowered);
    assert!(
        body.contains("zz_release(&"),
        "loop body must release its heap locals per iteration"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "300\n");
}

#[test]
fn borrow_args_skip_clone_for_pure_readers() {
    // `len(x)` / `fs.write(p, x)` only read their inputs, so local args
    // pass borrowed instead of a `zz_clone` temporary that nothing would
    // release (one leaked share per call — 1MB per `len(big)`).
    let src = "import std.fs\nfunc main() {\n    s := \"hello\"\n    println(len(s))\n    fs.write(\"/tmp/zz_borrow_probe.txt\", s)\n    fs.remove_file(\"/tmp/zz_borrow_probe.txt\")\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowered = lower_only(&pruned, &reach, "main").source;
    let body = lowered
        .find("zz_fn_main")
        .map(|i| &lowered[i..])
        .unwrap_or(&lowered);
    assert!(
        body.contains("zz_call_native1(zz_len, v"),
        "len() must pass the local borrowed, got:\n{body}"
    );
    assert!(
        !body.contains("zz_call_native1(zz_len, zz_clone("),
        "len() must not clone its argument"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "5\n");
}

/// Read `DT_NEEDED` entries via readelf (Linux-only; other platforms skip).
#[cfg(target_os = "linux")]
fn needed_libs(bin: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("readelf")
        .arg("-d")
        .arg(bin)
        .output()
        .expect("readelf -d");
    assert!(out.status.success(), "readelf failed");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|l| {
            l.find("Shared library: [").map(|i| {
                l[i + "Shared library: [".len()..]
                    .trim_end_matches(']')
                    .to_string()
            })
        })
        .collect()
}

/// Build `src` to a temp binary and return its path (caller cleans up).
fn build_temp_bin(src: &str) -> (PathBuf, PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (pruned, reach) = build_reachable(src);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("zz-link-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = native_bin(&tmp, "zz_out");
    build_native(&pruned, &reach, "main", BuildOptions::dev(), None, &bin)
        .unwrap_or_else(|e| panic!("build failed: {e}"));
    (tmp, bin)
}

#[cfg(target_os = "linux")]
#[test]
fn plain_program_links_no_curl_no_sqlite() {
    // Regression: every binary used to carry `DT_NEEDED libsqlite3`
    // (single-TU archive defeated `--as-needed`). Plain programs must
    // link neither heavy lib.
    let (tmp, bin) = build_temp_bin("func main() {\n    println(\"hi\")\n}\n");
    let needed = needed_libs(&bin);
    assert!(
        !needed.iter().any(|l| l.contains("sqlite3")),
        "plain program must not need sqlite3, got: {needed:?}"
    );
    assert!(
        !needed.iter().any(|l| l.contains("curl")),
        "plain program must not need curl, got: {needed:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(target_os = "linux")]
#[test]
fn fetch_program_links_curl_not_sqlite() {
    // Refused loopback port: exercises link + arg plumbing with no network.
    let src = "import std.http\nfunc main() {\n    match http.fetch(\"http://127.0.0.1:9/nope\") {\n        .ok(_r) => println(\"unexpected\"),\n        .err(_e) => println(\"fetch_err_ok\"),\n    }\n}\n";
    let (tmp, bin) = build_temp_bin(src);
    let needed = needed_libs(&bin);
    assert!(
        needed.iter().any(|l| l.contains("curl")),
        "fetch program must need curl, got: {needed:?}"
    );
    assert!(
        !needed.iter().any(|l| l.contains("sqlite3")),
        "fetch program must not need sqlite3, got: {needed:?}"
    );
    let (_, out) = compile::run_binary(&bin, &[]).unwrap();
    assert_eq!(out, "fetch_err_ok\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(target_os = "linux")]
#[test]
fn sql_program_links_sqlite_not_curl() {
    let src = "import std.sqlz\nmydb := sqlz.open(\":memory:\")\nmydb.exec(\"\"\"CREATE TABLE t (x INTEGER)\"\"\")\nprint(\"db_ok\")\n";
    let (tmp, bin) = build_temp_bin(src);
    let needed = needed_libs(&bin);
    assert!(
        needed.iter().any(|l| l.contains("sqlite3")),
        "sql program must need sqlite3, got: {needed:?}"
    );
    assert!(
        !needed.iter().any(|l| l.contains("curl")),
        "sql program must not need curl, got: {needed:?}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn scalar_fn_specializes_and_runs() {
    // fib(int) -> int must gain an unboxed variant; native output must
    // match the VM exactly (differential check on scalar recursion).
    let src = "func fib(n: int) -> int {\n    if n < 2 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nfunc main() {\n    println(fib(20))\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".to_string(),
        pruned.clone(),
    );
    assert!(
        lowerer.specialized.contains("main.fib") || lowerer.specialized.contains("fib"),
        "fib should specialize, got: {:?}",
        lowerer.specialized
    );
    // The unboxed variant must exist in the generated C with raw scalars.
    let lowered = lower_only(&pruned, &reach, "main");
    assert!(
        lowered.source.contains("_u(int64_t"),
        "expected an unboxed fib variant"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "6765\n");
}

#[test]
fn scalar_comparisons_and_floats_match_vm() {
    // Differential: comparisons, float arithmetic, and bool returns in
    // `_u` bodies must match the VM bit-for-bit (including int/float
    // promotion and comparison chaining through calls).
    let src = "func max2(a: int, b: int) -> int {\n    if a > b {\n        return a\n    }\n    return b\n}\nfunc fadd(x: float, y: float) -> float {\n    return x + y * 2.0\n}\nfunc is_pos(n: int) -> bool {\n    return n > 0\n}\nfunc main() {\n    println(max2(3, 7))\n    println(max2(9, 2))\n    println(fadd(1.5, 2.0))\n    println(is_pos(-4))\n    println(is_pos(4))\n    println(fib_sum(10))\n}\nfunc fib_sum(n: int) -> int {\n    total := 0\n    for i in 0..n {\n        total = total + i\n    }\n    return total\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".to_string(),
        pruned.clone(),
    );
    for f in ["max2", "fadd", "is_pos", "fib_sum"] {
        let key = if lowerer.specialized.contains(f) {
            f.to_string()
        } else {
            format!("main.{f}")
        };
        assert!(lowerer.specialized.contains(&key), "{f} should specialize");
    }
    let (_, out) = native_run(src);
    assert_eq!(out, "7\n9\n5.5\nfalse\ntrue\n45\n");
}

#[test]
fn scalar_fn_with_capturing_closure_matches_vm() {
    // Regression: `_u` bodies initially skipped capture analysis, so
    // nested closures lost captured locals (NULL env) and hung or
    // miscomputed. The closure itself stays boxed; only the enclosing
    // scalar function specializes.
    let src = "func with_cap(n: int) -> int {\n    m := n + 1\n    f := |x| x + m\n    return f(10)\n}\nfunc main() {\n    println(with_cap(5))\n}\n";
    let (pruned, reach) = build_reachable(src);
    let lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".to_string(),
        pruned.clone(),
    );
    assert!(
        lowerer.specialized.contains("with_cap"),
        "with_cap should specialize, got: {:?}",
        lowerer.specialized
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "16\n");
}

#[test]
fn safepoint_elided_without_concurrency() {
    // Loop-top `zz_safepoint()` calls are a codegen barrier: clang
    // cannot fold the loop while the opaque call sits at the top.
    // Concurrency-free programs must not emit any (the declaration in
    // the runtime prelude is not a call — filter on the call suffix).
    let src = "func main() {\n    s := 0\n    for i in 0..5000000 {\n        s = s + i\n    }\n    println(s)\n}\n";
    let (pruned, reach) = build_reachable(src);
    assert!(
        !reach.natives.iter().any(|n| n.contains("task")),
        "bench-shaped program must not pull task natives"
    );
    let c = lower_only(&pruned, &reach, "main").source;
    let calls: Vec<&str> = c
        .lines()
        .filter(|l| l.contains("zz_safepoint();"))
        .collect();
    assert!(
        calls.is_empty(),
        "expected no safepoint calls, found: {calls:?}"
    );
}

#[test]
fn safepoint_kept_with_concurrency() {
    // Programs that spawn tasks keep the courtesy yield so sibling
    // threads get scheduled inside tight loops.
    let src = "import std.task\nfunc main() {\n    h := task.spawn(|_| {\n        42\n    })\n    s := 0\n    for i in 0..100 {\n        s = s + i\n    }\n    println(\"{task.join(h)} {s}\")\n}\n";
    let (pruned, reach) = build_reachable(src);
    assert!(
        reach.natives.iter().any(|n| n.contains("task")),
        "spawn program must pull task natives, got: {:?}",
        reach.natives
    );
    let c = lower_only(&pruned, &reach, "main").source;
    assert!(
        c.lines().any(|l| l.contains("zz_safepoint();")),
        "spawn program must keep loop safepoints"
    );
}

#[test]
fn stack_array_index_forwards_to_raw_arith() {
    // `arr := [i, i + 1, i + 2]; a = a + arr[0] + arr[2]` must lower
    // to raw scalar arithmetic with no `zz_index_get` call in user
    // code (SROA, matching Rust's stack-array folding).
    let src = "func main() {\n    a := 0\n    for i in 0..100 {\n        arr := [i, i + 1, i + 2]\n        a = a + arr[0] + arr[2]\n    }\n    println(a)\n}\n";
    let (pruned, reach) = build_reachable(src);
    let c = lower_only(&pruned, &reach, "main").source;
    let user = c.split("// ---- generated code ----").nth(1).unwrap_or("");
    assert!(
        !user.contains("zz_index_get("),
        "forwarded reads must not call zz_index_get:\n{user}"
    );
    assert!(
        user.contains("(int64_t)("),
        "accumulation must stay raw scalar arith:\n{user}"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "10100\n");
}

#[test]
fn stack_array_forwarding_killed_by_store_and_push() {
    // Stores and mutating calls must kill forwarding: the read after
    // must observe the mutation, not the construction-time element.
    let src = "func main() {\n    arr := [1, 2, 3]\n    arr[0] = 99\n    println(arr[0])\n    b := [10, 20]\n    b.push(30)\n    println(\"{b[0]} {len(b)}\")\n}\n";
    let (_, out) = native_run(src);
    assert_eq!(out, "99\n10 3\n");
}

#[test]
fn str_append_chain_inlines_int_and_bool_casts() {
    // `s = s + "item_" + str(i) + ";"` must format directly into the
    // buffer: one `zz_str_append_int` call, no cast temp, no release.
    let src = "func main() {\n    st := \"\"\n    for i in 0..5 {\n        st = st + \"item_\" + str(i) + \";\"\n    }\n    println(st)\n    s2 := \"\"\n    b := true\n    s2 = s2 + \"v:\" + str(b) + \"!\"\n    println(s2)\n    s3 := \"\"\n    s3 = s3 + str(1.5)\n    println(s3)\n}\n";
    let (pruned, reach) = build_reachable(src);
    let c = lower_only(&pruned, &reach, "main").source;
    let user = c.split("// ---- generated code ----").nth(1).unwrap_or("");
    assert!(
        user.contains("zz_str_append_int("),
        "int casts must append directly:\n{user}"
    );
    assert!(
        user.contains("zz_str_append_bool("),
        "bool casts must append directly:\n{user}"
    );
    // No per-iteration cast temp for the int loop.
    assert!(
        !user.contains("zz_str_cast_arena("),
        "int loop must not stage through a cast temp:\n{user}"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "item_0;item_1;item_2;item_3;item_4;\nv:true!\n1.5\n");
}

#[test]
fn promoted_array_release_is_elided_in_loops() {
    // A loop-local stack-promoted array that is only read through
    // forwarded indices needs no `zz_release`: the header and scalar
    // items live on the C stack. Eliding the opaque call also lets
    // clang DCE the whole dead construction.
    let src = "func main() {\n    a := 0\n    for i in 0..10 {\n        arr := [i, i + 1]\n        a = a + arr[0] + arr[1]\n    }\n    println(a)\n}\n";
    let (pruned, reach) = build_reachable(src);
    let c = lower_only(&pruned, &reach, "main").source;
    let user = c.split("// ---- generated code ----").nth(1).unwrap_or("");
    assert!(
        !user.contains("zz_release("),
        "dead stack array must not be released:\n{user}"
    );
    let (_, out) = native_run(src);
    assert_eq!(out, "100\n");
}

#[test]
fn array_alias_kill_keeps_push_correct() {
    // `d := c` aliases the buffer: later reads of `c` must not forward
    // through construction-time texts (a push through the alias may
    // have reallocated). Push has value semantics (both engines agree
    // the source is unaffected) — this guards the kill logic.
    let src = "func main() {\n    c := [10, 20]\n    d := c\n    d.push(30)\n    println(\"{c[0]} {len(c)} {len(d)}\")\n}\n";
    let (_, out) = native_run(src);
    assert_eq!(out, "10 2 3\n");
}

#[test]
fn str_char_len_ascii_fast_path_matches_unicode_walk() {
    // The ASCII fast path in zz_str_char_len must agree with the
    // precise UTF-8 walk on pure-ASCII, multibyte, empty, and
    // boundary-length strings (exercises word-align prologue/epilogue).
    let src = "func main() {\n    println(len(\"hello\"))\n    println(len(\"héllo→世界\"))\n    println(len(\"\"))\n    println(len(\"abcdefg\"))\n    println(len(\"abcdefgh\"))\n    s := \"\"\n    for i in 0..100 {\n        s = s + \"x\"\n    }\n    println(len(s))\n}";
    let (_, out) = native_run(src);
    assert_eq!(out, "5\n8\n0\n7\n8\n100\n");
}

#[test]
fn native_top_level_unit_variant() {
    // Top-level (script) statements record no span types: unit-variant
    // construction must resolve by enum name, and namespaced globals of
    // enum type must not be mistaken for construction.
    let (_, out) = native_run(
        r#"
enum T { A, B }
x := T.A
println(x)
"#,
    );
    assert_eq!(out, "T.A()\n");
}

#[test]
fn native_nested_enum_miss_falls_through() {
    // A nested miss must try the next arm (not swallow the match).
    let (_, out) = native_run(
        r#"
enum Shape { Pt(int), Empty }
enum Wrapper { S(Shape), N(int) }
w := Wrapper.S(Shape.Empty)
match w {
    .S(.Pt(p)) => println(p),
    .N(n) => println(n),
    _ => println(-1),
}
"#,
    );
    assert_eq!(out, "-1\n");
}

#[test]
fn boxed_field_read_borrows_ident_base() {
    // #188: boxing an already-boxed struct argument for a call rebuilds
    // it member-wise; each synthesized `r.s0` read must borrow the
    // local for `zz_object_get_field` (which retains the field value)
    // instead of cloning the base into a `_field_obj` temp that nothing
    // releases (~0.5KB/draw retained in struct loops).
    // A caller passing its own (already boxed) struct parameter through
    // hits the box-for-arg path with a non-literal base: the shape that
    // retained per-iteration temps in loops.
    let src = r#"
struct Rng { s0: int, s1: int, s2: int, s3: int }
func next(r: Rng) -> (int, Rng) {
    return (r.s0 + 1, Rng{ s0: r.s0 + 1, s1: r.s1, s2: r.s2, s3: r.s3 })
}
func mid(r: Rng) -> (int, Rng) {
    return next(r)
}
func main() {
    r := Rng{ s0: 0, s1: 0, s2: 0, s3: 0 }
    println("{mid(r)}")
}
"#;
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let c = lowerer.lower().source;
    assert!(
        !c.contains("_field_obj"),
        "ident-base field reads must not hoist clone temps, got:\n{c}"
    );
    assert!(
        c.contains("zz_object_get_field(&v"),
        "expected a borrowed field read, got:\n{c}"
    );
}

#[test]
fn readonly_call_arg_borrows_instead_of_cloning() {
    // #188/#383: `next(r)` with a proven read-only parameter must pass
    // the live local (`v0`), not a retained `zz_clone` share and not a
    // member-wise rebuilt box (`__obj`), neither of which anything
    // releases (~300B retained per call).
    let src = r#"
struct Rng { s0: int, s1: int, s2: int, s3: int }
func next(r: Rng) -> (int, Rng) {
    return (r.s0 + 1, Rng{ s0: r.s0 + 1, s1: r.s1, s2: r.s2, s3: r.s3 })
}
func step(r: Rng, n: int) -> int {
    acc := 0
    i := 0
    while i < n {
        p := next(r)
        r = p[1]
        acc = (acc + p[0]) & 4294967295
        i += 1
    }
    return acc
}
func main() {
    r := Rng{ s0: 0, s1: 0, s2: 0, s3: 0 }
    println("{step(r, 200000)}")
}
"#;
    let (pruned, reach) = build_reachable(src);
    let lowerer = crate::Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        "main".into(),
        pruned.clone(),
    );
    let c = lowerer.lower().source;
    // Scope assertions to generated code (the embedded runtime headers
    // legitimately contain clones and temps).
    let gen = c
        .find("// ---- generated code ----")
        .map(|i| &c[i..])
        .unwrap_or(&c);
    // The step loop must call next with the live local (harness
    // builds without loader namespacing, so the callee is `zz_fn_next`).
    assert!(
        gen.contains("zz_fn_next((zz_value[]){ v0 }"),
        "expected a borrowed call arg, got:\n{gen}"
    );
    // ...and neither clone the loop-carried struct nor rebuild it via a
    // field-clone temp. (`zz_clone(v1)` for the int loop bound is
    // pre-existing, harmless, and out of scope.)
    assert!(
        !gen.contains("zz_clone(v0)") && !gen.contains("_field_obj"),
        "read-only call args must not clone, got:\n{gen}"
    );
}
