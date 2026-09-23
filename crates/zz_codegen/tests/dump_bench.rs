use std::collections::HashMap;
use zz_frontend::ast::Program;
use zz_frontend::span::Span;
use zz_hir::TypedProgram;
use zz_stdlib::stdlib_funcs;

fn load(src: &str) -> (TypedProgram, String) {
    let parsed = zz_frontend::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    // Merge stmts like CLI build does.
    let mut stmts = Vec::new();
    let merged_span = Span::new(0, 0);
    stmts.extend(parsed.program.stmts.iter().cloned());
    let merged = Program {
        stmts,
        span: merged_span,
    };
    let res = zz_hir::build_program(&merged, HashMap::new(), stdlib_funcs(), HashMap::new());
    let main_key = "bench_memory_arena.main".to_string();
    let (pruned, _reach) = zz_hir::dce(&res.program, &main_key);
    (pruned, main_key)
}

/// Dev utility: lower the arena bench and dump the generated C to
/// `/tmp/bench.c` for inspection. `#[ignore]`d because it asserts nothing
/// and reads from `examples/` (gitignored — absent in CI checkouts).
/// Run explicitly when needed:
///   cargo test -p zz_codegen --test dump_bench -- --ignored --nocapture
#[test]
#[ignore]
fn dump_bench_c() {
    let src_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/performace_check/arena/bench_memory_arena.zz");
    let src = std::fs::read_to_string(&src_path).unwrap();
    let (pruned, main_key) = load(&src);
    let lowerer = zz_codegen::Lowerer::new(
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
        main_key,
        pruned.clone(),
    );
    let lowered = lowerer.lower();
    let out = std::env::temp_dir().join("bench.c");
    std::fs::write(&out, &lowered.source).unwrap();
    println!("wrote {} ({} bytes)", out.display(), lowered.source.len());
}
