use std::collections::HashMap;
use std::path::Path;
use zz_checker::Type;
use zz_frontend::ast::Program;
use zz_frontend::span::Span;
use zz_hir::TypedProgram;
use zz_stdlib::stdlib_funcs;

fn load(src: &str) -> (TypedProgram, String) {
    let parsed = zz_frontend::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    // Merge stmts like CLI build does.
    let mut merged_stmts = Vec::new();
    let merged_span = Span::new(0, 0);
    for p in &[parsed.program] {
        merged_stmts.extend(p.stmts.iter().cloned());
    }
    let merged = Program {
        stmts: merged_stmts,
        span: merged_span,
    };
    let res = zz_hir::build_program(&merged, HashMap::new(), stdlib_funcs(), HashMap::new());
    let main_key = "bench_memory_arena.main".to_string();
    let (pruned, _reach) = zz_hir::dce(&res.program, &main_key);
    (pruned, main_key)
}

#[test]
fn dump_bench_c() {
    let src = std::fs::read_to_string(
        "/home/zaid/testing/zz_lang/examples/performace_check/arena/bench_memory_arena.zz",
    )
    .unwrap();
    let (pruned, main_key) = load(&src);
    let lowerer = zz_codegen::Lowerer::new(
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
        main_key,
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    std::fs::write("/tmp/bench.c", &lowered.source).unwrap();
    println!("wrote /tmp/bench.c ({} bytes)", lowered.source.len());
}
