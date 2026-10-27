//! Pure-ZZ stdlib home-module verification (#253): every embedded stdlib
//! program must compile, lower, and verify standalone. The chunk backend
//! merges these programs by reference (never by AST), so a regression
//! here breaks chunk coverage for every caller of that helper.

use std::sync::Arc;

fn standalone_verify(idx: usize, name: &str) {
    let prog = zz_stdlib::zz_stdlib_program_at(idx)
        .as_ref()
        .expect("stdlib module should compile");
    let natives: std::collections::HashSet<String> =
        zz_stdlib::stdlib_natives().keys().cloned().collect();
    let chunk = zz_runtime::vm::Compiler::compile_program_typed(
        &prog.program,
        Arc::new(prog.types.clone()),
        prog.structs.clone(),
        prog.enums.clone(),
        Arc::new(natives),
    );
    let module = lower::lower_typed(&chunk, &prog.funcs)
        .unwrap_or_else(|e| panic!("standalone {name} lower failed: {e}"));
    verify::verify(&module).unwrap_or_else(|e| panic!("standalone {name} verify failed: {e}"));
}

use zz_ir::{lower, verify};

#[test]
fn stdlib_str_verifies_standalone() {
    standalone_verify(0, "str");
}

#[test]
fn stdlib_csv_verifies_standalone() {
    standalone_verify(14, "csv");
}

#[test]
fn stdlib_all_programs_verify_standalone() {
    let names = [
        "str", "math", "vec", "json", "regexp", "time", "args", "colors", "path", "http", "map",
        "set", "dec", "bytes", "csv",
    ];
    for (idx, name) in names.iter().enumerate() {
        standalone_verify(idx, name);
    }
}
