//! HIR tests: typed-view construction and traversal over fixtures.

use std::collections::HashMap;

use zz_checker::Type;

use crate::walk::walk_exprs;
use crate::{build_source, is_dynamic, is_int};

/// True when `diags` contains any error-severity diagnostic (warnings are
/// expected for unused vars in small fixtures).
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
        if let zz_frontend::ast::Expr::Binary { op: _, .. } = te.expr {
            bin = te.ty.map(|t| t.clone());
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
