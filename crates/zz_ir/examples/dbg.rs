fn dump(c: &zz_runtime::vm::Chunk, name: &str) {
    println!(
        "--- chunk {name} (params={}, len={})",
        c.params.len(),
        c.code.len()
    );
    for (i, op) in c.code.iter().enumerate().take(8) {
        let s = format!("{op:?}");
        println!("  {i}: {}", &s[..s.len().min(100)]);
    }
    for op in &c.code {
        match op {
            zz_runtime::vm::Op::MakeFunc { name, chunk, .. } => {
                dump(chunk, &format!("func:{name}"))
            }
            zz_runtime::vm::Op::MakeClosure { chunk, .. } => dump(chunk, "closure"),
            zz_runtime::vm::Op::SpawnClosure { chunk, .. } => dump(chunk, "spawn"),
            _ => {}
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let src = std::fs::read_to_string(path).unwrap();
    let parsed = zz_frontend::parse(&src);
    let types = std::sync::Arc::new(std::collections::HashMap::new());
    let chunk = zz_runtime::vm::Compiler::compile_program_typed(
        &parsed.program,
        types,
        std::collections::HashMap::new(),
        std::collections::HashMap::new(),
        std::sync::Arc::new(std::collections::HashSet::new()),
    );
    dump(&chunk, "entry");
}
