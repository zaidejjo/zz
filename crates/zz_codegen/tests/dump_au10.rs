use std::collections::HashMap;
use zz_frontend::ast::Program;
use zz_frontend::span::Span;
use zz_hir::TypedProgram;
use zz_stdlib::stdlib_funcs;

fn load(src: &str) -> (TypedProgram, zz_hir::ReachableSet, String) {
    let parsed = zz_frontend::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let mut stmts = Vec::new();
    let merged_span = Span::new(0, 0);
    stmts.extend(parsed.program.stmts.iter().cloned());
    let merged = Program { stmts, span: merged_span };
    let res = zz_hir::build_program(&merged, HashMap::new(), stdlib_funcs(), HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let main_key = "main".to_string();
    let (pruned, reach) = zz_hir::dce(&res.program, &main_key);
    (pruned, reach, main_key)
}

#[test]
fn dump_matrix_full_c() {
    let src = r#"
import std.math
func main() {
    r1 := math.matrix_mul([[1.0, 2.0], [3.0, 4.0]], [[5.0, 6.0], [7.0, 8.0]])
    match r1 {
        .ok(m) => { println("rows={len(m)}"); println("cols={len(m[0])}"); }
        .err(e) => println("ERROR: {e}")
    }
}
"#;
    let (pruned, reach, main_key) = load(src);
    let lowerer = zz_codegen::Lowerer::new(reach.funcs.clone(), reach.natives.clone(), main_key, pruned.clone());
    let lowered = lowerer.lower();
    let out = std::env::temp_dir().join("matrix_full_lowered.c");
    std::fs::write(&out, &lowered.source).unwrap();
    println!("wrote {} ({} bytes)", out.display(), lowered.source.len());
}
