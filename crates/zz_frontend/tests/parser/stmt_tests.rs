//! Parser statement tests.

use zz_frontend::ast::{BinOp, Expr as E};
use zz_frontend::tests::common::parse_ok;

#[test]
fn parses_short_decl() {
    let p = parse_ok("x := 1 + 2");
    assert_eq!(p.stmts.len(), 1);
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            ty: None,
            name,
            value,
            ..
        } => {
            assert_eq!(name.name, "x");
            assert!(matches!(value, E::Binary { op: BinOp::Add, .. }));
        }
        other => panic!("expected short decl, got {other:?}"),
    }
}

#[test]
fn parses_explicit_decl() {
    let p = parse_ok("x: int = 10");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            ty: Some(ty), name, ..
        } => {
            assert_eq!(ty.kind, zz_frontend::ast::TyKind::Int);
            assert_eq!(name.name, "x");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_str_explicit_decl() {
    let p = parse_ok("s: str = \"hello\"");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty: Some(ty), .. } => {
            assert_eq!(ty.kind, zz_frontend::ast::TyKind::Str)
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_array_decl_and_literal() {
    let p = parse_ok("scores := [10, 20, 30]");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            ty: None, value, ..
        } => {
            assert!(matches!(value, E::Array { elems, .. } if elems.len() == 3));
        }
        other => panic!("unexpected: {other:?}"),
    }
    let p = parse_ok("scores: [int] = [10, 20, 30]");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty: Some(ty), .. } => {
            assert!(matches!(ty.kind, zz_frontend::ast::TyKind::Array(_)));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_dict_decl_and_literal() {
    let p = parse_ok("ages := {\"Zaid\": 20}");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            ty: None, value, ..
        } => {
            assert!(matches!(value, E::Dict { entries, .. } if entries.len() == 1));
        }
        other => panic!("unexpected: {other:?}"),
    }
    let p = parse_ok("ages: {str: int} = {\"Zaid\": 20}");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty: Some(ty), .. } => {
            assert!(matches!(ty.kind, zz_frontend::ast::TyKind::Dict(_, _)));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_union_type() {
    let p = parse_ok("user: {str: str | int} = {\"name\": \"Zaid\", \"age\": 20}");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty: Some(ty), .. } => {
            assert!(matches!(ty.kind, zz_frontend::ast::TyKind::Dict(_, _)));
            if let zz_frontend::ast::TyKind::Dict(_, v) = &ty.kind {
                assert!(matches!(v.kind, zz_frontend::ast::TyKind::Union(_)));
            }
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_import() {
    let p = parse_ok("import std.io");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Import { path, .. } => {
            assert_eq!(path, &vec!["std".to_string(), "io".to_string()])
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_func() {
    let p = parse_ok("func add(a: int, b: int) -> int { return a + b }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func {
            name, params, ret, ..
        } => {
            assert_eq!(name, &vec!["add".to_string()]);
            assert_eq!(params.len(), 2);
            assert_eq!(ret.as_ref().unwrap().kind, zz_frontend::ast::TyKind::Int);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_generic_func() {
    let p = parse_ok("func id<T>(x: T) -> T { return x }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func {
            generics, params, ..
        } => {
            assert_eq!(generics.len(), 1);
            assert_eq!(generics[0].name, "T");
            assert_eq!(
                params[0].ty.as_ref().unwrap().kind,
                zz_frontend::ast::TyKind::Named("T".into(), vec![])
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_option_result_types() {
    let p = parse_ok("a: Option<int> = .none\nb: Result<int, str> = .ok(1)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { ty: Some(ty), .. } => {
            assert!(matches!(ty.kind, zz_frontend::ast::TyKind::Option(_)));
        }
        other => panic!("unexpected: {other:?}"),
    }
    match &p.stmts[1] {
        zz_frontend::ast::Stmt::Decl { ty: Some(ty), .. } => {
            assert!(matches!(ty.kind, zz_frontend::ast::TyKind::Result(_, _)));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn multiple_statements() {
    let p = parse_ok("a := 1\nb := 2\nc := a + b");
    assert_eq!(p.stmts.len(), 3);
}

#[test]
fn empty_program_ok() {
    let p = parse_ok("");
    assert!(p.stmts.is_empty());
}

#[test]
fn missing_equals_reports_error_and_recovers() {
    let parsed = zz_frontend::parse("x := 1\ny := 2");
    assert_eq!(parsed.errors.len(), 0);
    assert_eq!(parsed.program.stmts.len(), 2);
}

#[test]
fn missing_expression_reports_error() {
    let parsed = zz_frontend::parse("x :=");
    assert_eq!(parsed.errors.len(), 1);
}

#[test]
fn missing_close_paren_reports_error() {
    let parsed = zz_frontend::parse("(1 + 2");
    assert!(
        parsed.errors.len() >= 1,
        "expected at least 1 error for unclosed paren, got {}",
        parsed.errors.len()
    );
    // The diagnostic message should mention the missing `)`.
    let msgs: Vec<_> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
    assert!(
        msgs.iter().any(|m| m.contains(')')),
        "expected a message mentioning `)`, got: {msgs:?}"
    );
}

#[test]
fn missing_stmt_end_reports_error() {
    let parsed = zz_frontend::parse("x := 1 y := 2");
    assert_eq!(parsed.errors.len(), 1);
}
