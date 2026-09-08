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
    let res = zz_hir::build_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
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
        &bin,
    )
    .unwrap_or_else(|e| panic!("build failed: {e}"));
    let r = zz_codegen::compile::run_binary(&bin, &[]).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    r
}
