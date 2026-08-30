//! Parser declaration/type tests.

use zz_frontend::ast::Expr as E;
use zz_frontend::tests::common::parse_ok;

#[test]
fn parses_struct() {
    let p = parse_ok("struct Point { x: int, y: int }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Struct { name, fields, .. } => {
            assert_eq!(name, &vec!["Point".to_string()]);
            assert_eq!(fields.len(), 2);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_for() {
    let p = parse_ok("for x in xs { y := 1 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::For {
            var, iter, body, ..
        } => {
            assert_eq!(var.name, "x");
            assert!(matches!(iter.as_ref(), E::Ident { name, .. } if name == "xs"));
            assert_eq!(body.stmts.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_break_continue() {
    let p = parse_ok("break\ncontinue");
    assert_eq!(p.stmts.len(), 2);
    assert!(matches!(p.stmts[0], zz_frontend::ast::Stmt::Break { .. }));
    assert!(matches!(
        p.stmts[1],
        zz_frontend::ast::Stmt::Continue { .. }
    ));
}

#[test]
fn parses_defer() {
    let p = parse_ok("defer f()");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Defer { expr, .. } => {
            assert!(matches!(expr.as_ref(), E::Call { .. }));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_assign() {
    let p = parse_ok("x = 5");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Assign { target, value, .. } => {
            assert!(matches!(target, E::Ident { name, .. } if name == "x"));
            assert!(matches!(value, E::Int { value: 5, .. }));
        }
        other => panic!("unexpected: {other:?}"),
    }
}
