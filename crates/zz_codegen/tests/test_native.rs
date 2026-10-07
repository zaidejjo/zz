#[test]
fn test_string_contains_top_level() {
    let src = r#"
s := "Hello, World!"
println(s.contains("World"))
println(s.contains("Missing"))
println(s.contains(""))
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "true\nfalse\ntrue\n",
        "native output mismatch: {stdout}"
    );
}

#[test]
fn test_string_contains_func_main() {
    let src = r#"
func main() {
    s := "Hello, World!"
    println(s.contains("World"))
    println(s.contains("Missing"))
    println(s.contains(""))
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "true\nfalse\ntrue\n",
        "native output mismatch: {stdout}"
    );
}

fn native_run(src: &str) -> (i32, String) {
    use std::collections::HashMap;
    let parsed = zz_frontend::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let funcs = zz_stdlib::stdlib_funcs();
    let res = zz_hir::build_program(
        &parsed.program,
        HashMap::new(),
        funcs,
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
    );
    let tp = res.program;
    let (pruned, reach) = zz_hir::dce(&tp, "main");
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("zz-test-bug-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let bin = tmp.join("zz_out");
    zz_codegen::build_native(
        &pruned,
        &reach,
        "main",
        zz_codegen::BuildOptions::dev(),
        None,
        &bin,
    )
    .unwrap_or_else(|e| panic!("build failed: {e}"));
    let r = zz_codegen::compile::run_binary(&bin, &[]).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

#[test]
fn test_struct_return_boxed() {
    let src = r#"
struct Counts { files: int, lines: int }
func zero() -> Counts {
    Counts{ files: 0, lines: 0 }
}
func main() {
    c := zero()
    println("files={c.files} lines={c.lines}")
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "files=0 lines=0\n",
        "native output mismatch: {stdout}"
    );
}

#[test]
fn test_struct_field_mutation_through_call_result() {
    let src = r#"
struct Counts { files: int, lines: int }
func zero() -> Counts {
    Counts{ files: 0, lines: 0 }
}
func bump(n: int) -> Counts {
    c := zero()
    c.files = c.files + n
    c.lines = c.lines + 1
    c
}
func main() {
    c := bump(5)
    println("files={c.files} lines={c.lines}")
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "files=5 lines=1\n",
        "native output mismatch: {stdout}"
    );
}

#[test]
fn test_struct_alias_copies_like_vm() {
    let src = r#"
struct P { x: int }
func main() {
    a := P{ x: 1 }
    b := a
    b.x = 99
    println("{a.x} {b.x}")
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(stdout, "1 99\n", "native output mismatch: {stdout}");
}

#[test]
fn test_and_or_with_bool_locals() {
    let src = r#"
func main() {
    a := true
    b := false
    if a && !b {
        println("and_ok")
    }
    x := 5
    y := 10
    if x < y && y > 0 {
        println("cmp_and_ok")
    }
    if b || a {
        println("or_ok")
    }
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "and_ok\ncmp_and_ok\nor_ok\n",
        "native output mismatch: {stdout}"
    );
}

#[test]
fn test_match_arm_block_with_typed_decl() {
    let src = r#"
func main() {
    r := "42" |> int()
    match r {
        .none => println("got none")
        .some(id) => {
            xs: [int] = [id, 2, 3]
            println("len={len(xs)} first={xs[0]}")
        }
    }
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "len=3 first=42\n",
        "native output mismatch: {stdout}"
    );
}

#[test]
fn test_tail_if_multi_statement_branches() {
    let src = r#"
func pick(n: int) -> int {
    if n > 0 {
        println("pos")
        n * 2
    } else {
        println("neg")
        0 - n
    }
}
func main() {
    println("{pick(21)}")
    println("{pick(-21)}")
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(
        stdout, "pos\n42\nneg\n21\n",
        "native output mismatch: {stdout}"
    );
}

#[test]
fn test_nul_needle_contains_is_false() {
    let src = r#"
import std.str
func main() {
    println("{str.contains("hello", "\x00")}")
    println("{str.contains("he\x00llo", "\x00")}")
}
"#;
    let (exit, stdout) = native_run(src);
    assert_eq!(exit, 0, "native failed");
    assert_eq!(stdout, "false\ntrue\n", "native output mismatch: {stdout}");
}
