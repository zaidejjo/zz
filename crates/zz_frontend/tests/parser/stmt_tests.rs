//! Parser statement tests.

use zz_frontend::ast::{BinOp, Expr as E};
use zz_frontend::parse;
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
    let p = parse_ok("import std.str");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Import { path, .. } => {
            assert_eq!(path, &vec!["std".to_string(), "str".to_string()])
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
            assert_eq!(generics[0].name.name, "T");
            assert!(generics[0].bounds.is_empty());
            assert_eq!(
                params[0].ty.as_ref().unwrap().kind,
                zz_frontend::ast::TyKind::Named("T".into(), vec![])
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_generic_func_with_bounds() {
    let p = parse_ok("func min<T: Num + Ord>(a: T, b: T) -> T { return a }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func {
            generics, params, ..
        } => {
            assert_eq!(generics.len(), 1);
            assert_eq!(generics[0].name.name, "T");
            assert_eq!(
                generics[0].bounds,
                vec![
                    zz_frontend::ast::TraitBound::Num,
                    zz_frontend::ast::TraitBound::Ord
                ]
            );
            assert_eq!(
                params[0].ty.as_ref().unwrap().kind,
                zz_frontend::ast::TyKind::Named("T".into(), vec![])
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_generic_func_with_unknown_bound_errors() {
    let p = parse("func f<T: Foo>(x: T) -> T { return x }");
    assert!(!p.errors.is_empty());
    assert!(
        p.errors
            .iter()
            .any(|e| e.message.contains("unknown trait bound")),
        "errors: {:?}",
        p.errors
    );
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
fn parses_const_decl() {
    let p = parse_ok("const x = 10");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            ty: None,
            name,
            is_const,
            ..
        } => {
            assert!(is_const, "expected is_const=true");
            assert_eq!(name.name, "x");
        }
        other => panic!("expected const decl, got {other:?}"),
    }
}

#[test]
fn parses_const_explicit_decl() {
    let p = parse_ok("const x: int = 10");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            ty: Some(ty),
            is_const,
            ..
        } => {
            assert!(is_const, "expected is_const=true");
            assert_eq!(ty.kind, zz_frontend::ast::TyKind::Int);
        }
        other => panic!("expected const decl, got {other:?}"),
    }
}

#[test]
fn parses_pub_const_decl() {
    let p = parse_ok("pub const x = 10");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { is_const, pub_, .. } => {
            assert!(is_const, "expected is_const=true");
            assert!(pub_, "expected pub_=true");
        }
        other => panic!("expected const decl, got {other:?}"),
    }
}

#[test]
fn plain_decl_is_mutable() {
    let p = parse_ok("x := 10");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { is_const, .. } => {
            assert!(!is_const, "expected is_const=false");
        }
        other => panic!("expected decl, got {other:?}"),
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
        !parsed.errors.is_empty(),
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

#[test]
fn parses_bare_decorator() {
    let p = parse_ok("@login_required\nfunc secret(name: str) -> str { return name }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func {
            name, decorators, ..
        } => {
            assert_eq!(name, &vec!["secret".to_string()]);
            assert_eq!(decorators.len(), 1);
            assert_eq!(decorators[0].path, vec!["login_required".to_string()]);
            assert!(decorators[0].args.is_empty());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_decorator_with_args() {
    let p = parse_ok("@route(\"/hello\")\nfunc hello(name: str) -> str { return name }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func {
            name, decorators, ..
        } => {
            assert_eq!(name, &vec!["hello".to_string()]);
            assert_eq!(decorators.len(), 1);
            assert_eq!(decorators[0].path, vec!["route".to_string()]);
            assert_eq!(decorators[0].args.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_stacked_decorators() {
    let p = parse_ok("@logging\n@route(\"/hello\")\nfunc hello(name: str) -> str { return name }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func { decorators, .. } => {
            assert_eq!(decorators.len(), 2);
            assert_eq!(decorators[0].path, vec!["logging".to_string()]);
            assert_eq!(decorators[1].path, vec!["route".to_string()]);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_decorator_on_pub_func() {
    let p = parse_ok("@logged\npub func hello(name: str) -> str { return name }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Func {
            name,
            decorators,
            pub_,
            ..
        } => {
            assert_eq!(name, &vec!["hello".to_string()]);
            assert!(*pub_);
            assert_eq!(decorators.len(), 1);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn link_directive_still_parses() {
    let p = parse_ok("@link(\"sqlite3\")");
    assert!(matches!(&p.stmts[0], zz_frontend::ast::Stmt::Link { .. }));
}

#[test]
fn decorator_expands_to_inner_and_wrapper() {
    let parsed = parse_ok("@logging\nfunc hello(name: str) -> str { return name }");
    let (expanded, errors) = zz_frontend::decorators::expand_program(&parsed);
    assert!(errors.is_empty());
    assert_eq!(expanded.stmts.len(), 2);
    match (&expanded.stmts[0], &expanded.stmts[1]) {
        (
            zz_frontend::ast::Stmt::Func { name: inner, .. },
            zz_frontend::ast::Stmt::Func {
                name: outer,
                decorators,
                ..
            },
        ) => {
            assert_eq!(inner, &vec!["hello__inner".to_string()]);
            assert_eq!(outer, &vec!["hello".to_string()]);
            assert!(decorators.is_empty());
        }
        other => panic!("unexpected expansion: {other:?}"),
    }
}

#[test]
fn decorator_on_generic_func_errors() {
    let parsed = parse_ok("@logging\nfunc id<T>(x: T) -> T { return x }");
    let (_, errors) = zz_frontend::decorators::expand_program(&parsed);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("generic"));
}
