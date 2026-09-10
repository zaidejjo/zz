//! Parser declaration/type tests.

use zz_frontend::ast::{Expr as E, TyKind};
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
            vars, iter, body, ..
        } => {
            assert_eq!(vars.len(), 1);
            assert_eq!(vars[0].name, "x");
            assert!(matches!(iter.as_ref(), E::Ident { name, .. } if name == "xs"));
            assert_eq!(body.stmts.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_for_with_two_vars() {
    let p = parse_ok("for k, v in d { println(k) }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::For {
            vars, iter, body, ..
        } => {
            assert_eq!(vars.len(), 2);
            assert_eq!(vars[0].name, "k");
            assert_eq!(vars[1].name, "v");
            assert!(matches!(iter.as_ref(), E::Ident { name, .. } if name == "d"));
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

// --- Function type tests ---

#[test]
fn parses_func_type_keyword() {
    let p = parse_ok("f: func(int) -> int = x");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Func(params, ret) => {
                    assert_eq!(params.len(), 1);
                    assert!(matches!(params[0].kind, TyKind::Int));
                    assert!(matches!(ret.kind, TyKind::Int));
                }
                other => panic!("expected Func, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func_type_multi_params() {
    let p = parse_ok("f: func(int, str) -> bool = x");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Func(params, ret) => {
                    assert_eq!(params.len(), 2);
                    assert!(matches!(params[0].kind, TyKind::Int));
                    assert!(matches!(params[1].kind, TyKind::Str));
                    assert!(matches!(ret.kind, TyKind::Bool));
                }
                other => panic!("expected Func, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func_type_no_params() {
    let p = parse_ok("f: func() -> int = x");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Func(params, ret) => {
                    assert_eq!(params.len(), 0);
                    assert!(matches!(ret.kind, TyKind::Int));
                }
                other => panic!("expected Func, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func_type_shorthand() {
    let p = parse_ok("f: (int) -> int = x");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Func(params, ret) => {
                    assert_eq!(params.len(), 1);
                    assert!(matches!(params[0].kind, TyKind::Int));
                    assert!(matches!(ret.kind, TyKind::Int));
                }
                other => panic!("expected Func, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func_type_shorthand_multi() {
    let p = parse_ok("f: (int, str) -> bool = x");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Func(params, ret) => {
                    assert_eq!(params.len(), 2);
                    assert!(matches!(params[0].kind, TyKind::Int));
                    assert!(matches!(params[1].kind, TyKind::Str));
                    assert!(matches!(ret.kind, TyKind::Bool));
                }
                other => panic!("expected Func, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func_type_returning_func() {
    let p = parse_ok("f: func(int) -> func(int) -> int = x");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Func(params, ret) => {
                    assert_eq!(params.len(), 1);
                    assert!(matches!(params[0].kind, TyKind::Int));
                    match &ret.kind {
                        TyKind::Func(inner_params, inner_ret) => {
                            assert_eq!(inner_params.len(), 1);
                            assert!(matches!(inner_params[0].kind, TyKind::Int));
                            assert!(matches!(inner_ret.kind, TyKind::Int));
                        }
                        other => panic!("expected inner Func, got {other:?}"),
                    }
                }
                other => panic!("expected Func, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func_type_in_func_param() {
    let p = parse_ok("func apply(f: func(int) -> int, x: int) -> int { f(x) }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func { params, ret, .. } => {
            assert_eq!(params.len(), 2);
            match &params[0].ty.as_ref().unwrap().kind {
                TyKind::Func(ps, r) => {
                    assert_eq!(ps.len(), 1);
                    assert!(matches!(ps[0].kind, TyKind::Int));
                    assert!(matches!(r.kind, TyKind::Int));
                }
                other => panic!("expected Func param type, got {other:?}"),
            }
            assert!(matches!(ret.as_ref().unwrap().kind, TyKind::Int));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn paren_type_still_parses_as_tuple() {
    let p = parse_ok("x: (int, str) = y");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            match &ty.kind {
                TyKind::Tuple(ts) => {
                    assert_eq!(ts.len(), 2);
                    assert!(matches!(ts[0].kind, TyKind::Int));
                    assert!(matches!(ts[1].kind, TyKind::Str));
                }
                other => panic!("expected Tuple, got {other:?}"),
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn paren_type_still_parses_as_grouped() {
    let p = parse_ok("x: (int) = y");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty, .. } => {
            let ty = ty.as_ref().expect("expected type annotation");
            assert!(matches!(ty.kind, TyKind::Int));
        }
        other => panic!("unexpected: {other:?}"),
    }
}
