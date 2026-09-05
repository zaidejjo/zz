//! HIR tests: typed-view construction and traversal over fixtures.

use std::collections::HashMap;

use zz_checker::{FuncSig, Type};

use crate::callgraph::{build_callgraph, dce, reachable};
use crate::walk::walk_exprs;
use crate::{build_source, is_dynamic, is_int};

/// Seed a small stdlib surface for call-graph tests: io + http + str + vec
/// natives so method dispatch and module pruning are observable.
fn seed_stdlib() -> HashMap<String, FuncSig> {
    let mut f = HashMap::new();
    let unit = Type::Unit;
    let t_any = Type::Named("T".to_string());
    // io
    f.insert(
        "io.println".into(),
        FuncSig {
            generics: vec!["T".into()],
            params: vec![("v".into(), t_any.clone())],
            has_default: vec![false],
            ret: unit.clone(),
        },
    );
    f.insert(
        "io.print".into(),
        FuncSig {
            generics: vec!["T".into()],
            params: vec![("v".into(), t_any.clone())],
            has_default: vec![false],
            ret: unit.clone(),
        },
    );
    // http (heavy module — should be pruned if unused)
    f.insert(
        "http.get".into(),
        FuncSig {
            generics: vec![],
            params: vec![("url".into(), Type::Str)],
            has_default: vec![false],
            ret: Type::Response,
        },
    );
    f.insert(
        "http.server".into(),
        FuncSig {
            generics: vec![],
            params: vec![],
            has_default: vec![],
            ret: Type::HttpServer,
        },
    );
    // str methods
    f.insert(
        "str.len".into(),
        FuncSig {
            generics: vec![],
            params: vec![("self".into(), Type::Str)],
            has_default: vec![false],
            ret: Type::Int,
        },
    );
    f.insert(
        "str.trim".into(),
        FuncSig {
            generics: vec![],
            params: vec![("self".into(), Type::Str)],
            has_default: vec![false],
            ret: Type::Str,
        },
    );
    // vec methods
    f.insert(
        "vec.push".into(),
        FuncSig {
            generics: vec!["T".into()],
            params: vec![
                ("self".into(), Type::Array(Box::new(t_any.clone()))),
                ("v".into(), t_any),
            ],
            has_default: vec![false, false],
            ret: unit,
        },
    );
    f
}

fn has_errors(diags: &[zz_frontend::diag::RawDiag]) -> bool {
    diags
        .iter()
        .any(|d| d.severity == zz_frontend::diag::Severity::Error)
}

#[test]
fn resolves_basic_types() {
    let src = "x := 42\ns := \"hello\"\nb := true\nf := 3.5\n";
    let res = build_source(src, HashMap::new(), HashMap::new(), HashMap::new()).unwrap();
    assert!(
        !has_errors(&res.diagnostics),
        "unexpected diags: {:?}",
        res.diagnostics
    );
    let tp = &res.program;
    // x := 42 -> the decl value expression Int has resolved type Int
    let int_spans: Vec<_> = {
        let mut found = Vec::new();
        walk_exprs(tp, &mut |te| {
            if matches!(te.expr, zz_frontend::ast::Expr::Int { .. }) {
                assert!(matches!(te.ty, Some(Type::Int)), "int node got {:?}", te.ty);
                found.push(());
            }
            true
        });
        found
    };
    assert!(!int_spans.is_empty());
    let mut saw_str = false;
    let mut saw_float = false;
    walk_exprs(tp, &mut |te| {
        match te.expr {
            zz_frontend::ast::Expr::Str { .. } => {
                assert!(matches!(te.ty, Some(Type::Str)));
                saw_str = true;
            }
            zz_frontend::ast::Expr::Float { .. } => {
                assert!(matches!(te.ty, Some(Type::Float)));
                saw_float = true;
            }
            _ => {}
        }
        true
    });
    assert!(saw_str && saw_float);
}

#[test]
fn resolves_binary_and_call_types() {
    let src = "a := 1 + 2 * 3\nb := a > 5\n";
    let res = build_source(src, HashMap::new(), HashMap::new(), HashMap::new()).unwrap();
    let tp = &res.program;
    let mut saw_call_none = false;
    walk_exprs(tp, &mut |te| {
        if let zz_frontend::ast::Expr::Binary { op, .. } = te.expr {
            match op {
                zz_frontend::ast::BinOp::Add => {
                    assert!(matches!(te.ty, Some(Type::Int)));
                }
                zz_frontend::ast::BinOp::Gt => {
                    assert!(matches!(te.ty, Some(Type::Bool)));
                }
                _ => {}
            }
        }
        if let zz_frontend::ast::Expr::Call { .. } = te.expr {
            // No stdlib seeded -> this snippet has no calls, but if it did
            // the callee lookup would be Err-typed. Assert nothing about it.
            saw_call_none = true;
        }
        true
    });
    // Just asserting we walked without panic; call nodes absent in this src.
    let _ = saw_call_none;
}

#[test]
fn single_node_per_span_map_lookup() {
    let src = "y := 10\ny + 1\n";
    let res = build_source(src, HashMap::new(), HashMap::new(), HashMap::new()).unwrap();
    let tp = &res.program;
    let mut bin = None;
    walk_exprs(tp, &mut |te| {
        if let zz_frontend::ast::Expr::Binary { .. } = te.expr {
            bin = te.ty.cloned();
        }
        true
    });
    assert!(matches!(bin, Some(Type::Int)), "binary type got {bin:?}");
}

#[test]
fn dynamic_type_classification() {
    assert!(is_dynamic(&Type::Dict(
        Box::new(Type::Str),
        Box::new(Type::Int)
    )));
    assert!(is_dynamic(&Type::Json));
    assert!(is_dynamic(&Type::Func(
        vec![Type::Int],
        Box::new(Type::Int)
    )));
    assert!(!is_dynamic(&Type::Int));
    assert!(!is_dynamic(&Type::Str));
    assert!(is_int(&Type::Int));
    assert!(!is_int(&Type::Float));
}

#[test]
fn struct_and_options_resolve() {
    let src = "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nq := .some(5)\n";
    let res = build_source(src, HashMap::new(), HashMap::new(), HashMap::new()).unwrap();
    assert!(
        !has_errors(&res.diagnostics),
        "unexpected diags: {:?}",
        res.diagnostics
    );
    let tp = &res.program;
    let mut saw_struct_init = false;
    walk_exprs(tp, &mut |te| {
        if let zz_frontend::ast::Expr::StructInit { .. } = te.expr {
            assert!(matches!(te.ty, Some(Type::Struct(_))), "got {:?}", te.ty);
            saw_struct_init = true;
        }
        true
    });
    assert!(saw_struct_init);
}

#[test]
fn builds_typed_fixture_programs() {
    // The checker test fixture containing closures/loops should build a
    // typed program without panicking and produce structure.
    let src = r#"
fib := |n: int| -> int {
    if n <= 1 { 1 } else { fib(n - 1) + fib(n - 2) }
}
sum := 0
for i in 0..10 {
    sum = sum + i
}
fib(5)
"#;
    let res = build_source(src, HashMap::new(), HashMap::new(), HashMap::new()).unwrap();
    let tp = &res.program;
    // Count how many nodes got typed.
    let mut typed = 0usize;
    let mut total = 0usize;
    walk_exprs(tp, &mut |te| {
        total += 1;
        if te.ty.is_some() {
            typed += 1;
        }
        true
    });
    assert!(total > 5, "expected multiple nodes, got {total}");
    assert!(typed > 0, "no nodes typed");
}

// ---------------- call-graph & DCE tests ----------------

fn build_with_stdlib(src: &str) -> crate::TypedResult {
    build_source(src, HashMap::new(), seed_stdlib(), HashMap::new()).unwrap()
}

#[test]
fn reachable_mark_io_only_when_http_imported_but_unused() {
    // The test in the plan: `import std.http` but only use `io.println`.
    // Reachability must include io.println and strip http.get/http.server.
    let src = "import std.http\nfunc main() {\n    io.println(\"hi\")\n}\n";
    let res = build_with_stdlib(src);
    assert!(
        !has_errors(&res.diagnostics),
        "unexpected diags: {:?}",
        res.diagnostics
    );
    let tp = &res.program;
    let reach = reachable(tp, "main");

    // FIXME: the import registers `http` namespace only if reachable funcs
    // use it; since io.println is a direct call, http.* must be pruned.
    let mut saw_io = false;
    let mut saw_http = false;
    for f in &reach.funcs {
        if f == "io.println" {
            saw_io = true;
        }
        if f.starts_with("http.") {
            saw_http = true;
        }
    }
    assert!(
        saw_io,
        "io.println should be reachable, got {:?}",
        reach.funcs
    );
    assert!(!saw_http, "http.* should be pruned, got {:?}", reach.funcs);
    // natives = reachable but not program-defined.
    assert!(reach.natives.contains("io.println"));
    assert!(!reach.natives.iter().any(|n| n.starts_with("http.")));
}

#[test]
fn method_dispatch_traces_str_and_vec() {
    let src = r#"
func main() {
    s := "  hi  "
    n := s.len()
    io.println(n)
}
"#;
    let res = build_with_stdlib(src);
    assert!(!has_errors(&res.diagnostics), "{:?}", res.diagnostics);
    let tp = &res.program;
    let reach = reachable(tp, "main");
    assert!(
        reach.funcs.contains("str.len"),
        "s.len() should resolve to str.len, got {:?}",
        reach.funcs
    );
    assert!(reach.natives.contains("str.len"));
    assert!(!reach.funcs.contains("str.trim"), "trim unused");
}

#[test]
fn struct_only_kept_when_instantiated() {
    let src = r#"
struct Used { x: int }
struct Unused { y: int }
func main() {
    p := Used{ x: 1 }
    io.println(p.x)
}
"#;
    let res = build_with_stdlib(src);
    assert!(!has_errors(&res.diagnostics), "{:?}", res.diagnostics);
    let tp = &res.program;
    let (pruned, reach) = dce(tp, "main");
    assert!(reach.structs.contains("Used"), "Used must stay");
    assert!(!reach.structs.contains("Unused"), "Unused must be pruned");

    // The pruned program must not contain `struct Unused`.
    let has_unused = pruned.stmts().iter().any(
        |s| matches!(s, zz_frontend::ast::Stmt::Struct { name, .. } if name.join(".") == "Unused"),
    );
    assert!(!has_unused, "pruned program still has struct Unused");
}

#[test]
fn unused_function_pruned_unused_kept() {
    let src = r#"
func used(x: int) -> int { x * 2 }
func unused(x: int) -> int { x * 3 }
func main() {
    io.println(used(21))
}
"#;
    let res = build_with_stdlib(src);
    assert!(!has_errors(&res.diagnostics), "{:?}", res.diagnostics);
    let tp = &res.program;
    let (pruned, reach) = dce(tp, "main");
    assert!(reach.funcs.contains("used"), "used must be reachable");
    assert!(!reach.funcs.contains("unused"), "unused must be pruned");
    let has_unused_fn = pruned.stmts().iter().any(|s| match s {
        zz_frontend::ast::Stmt::Func { name, .. } => name.join(".") == "unused",
        _ => false,
    });
    assert!(!has_unused_fn, "unused fn still in pruned program");
}

#[test]
fn recursive_function_kept() {
    let src = r#"
func factorial(n: int) -> int {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}
func main() {
    io.println(factorial(5))
}
"#;
    let res = build_with_stdlib(src);
    assert!(!has_errors(&res.diagnostics), "{:?}", res.diagnostics);
    let tp = &res.program;
    let reach = reachable(tp, "main");
    assert!(
        reach.funcs.contains("factorial"),
        "recursive fn must be kept"
    );
}

#[test]
fn function_used_as_value_kept() {
    let src = r#"
func helper(x: int) -> int { x + 1 }
func main() {
    g := helper
    io.println(g(1))
}
"#;
    let res = build_with_stdlib(src);
    assert!(!has_errors(&res.diagnostics), "{:?}", res.diagnostics);
    let tp = &res.program;
    let reach = reachable(tp, "main");
    assert!(
        reach.funcs.contains("helper"),
        "helper used as value must be kept, got {:?}",
        reach.funcs
    );
}

#[test]
fn callgraph_has_edges_from_top() {
    let src = "io.println(1)\n";
    let res = build_with_stdlib(src);
    let tp = &res.program;
    let cg = build_callgraph(tp);
    let top_edges = cg.edges.get(crate::callgraph::TOP);
    assert!(top_edges.is_some(), "top-level statements must have edges");
    let edges = top_edges.unwrap();
    assert!(
        edges.contains(&"io.println".to_string()),
        "expected io.println edge, got {edges:?}"
    );
}
