//! Parser expression tests.

use zz_frontend::ast::{BinOp, Expr as E, UnOp};
use zz_frontend::tests::common::parse_ok;

#[test]
fn precedence_multiplication_over_addition() {
    let p = parse_ok("1 + 2 * 3");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Binary {
            op: BinOp::Add,
            left,
            right,
            ..
        }) => {
            assert!(matches!(**left, E::Int { value: 1, .. }));
            assert!(matches!(**right, E::Binary { op: BinOp::Mul, .. }));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn comparison_precedence() {
    let p = parse_ok("1 + 2 < 3 * 4");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Binary { op: BinOp::Lt, .. }) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn logical_ops() {
    let p = parse_ok("true && false || !true");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Binary { op: BinOp::Or, .. }) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parens_override_precedence() {
    let p = parse_ok("(1 + 2) * 3");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Binary {
            op: BinOp::Mul,
            left,
            ..
        }) => {
            assert!(matches!(**left, E::Paren { .. }));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn unary_minus() {
    let p = parse_ok("-5");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Unary { op: UnOp::Neg, .. }) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_call() {
    let p = parse_ok("add(1, 2)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Call { args, .. }) => assert_eq!(args.len(), 2),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_closure() {
    let p = parse_ok("|x: int, y| x + y");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Closure { params, .. }) => {
            assert_eq!(params.len(), 2);
            assert!(params[0].ty.is_some());
            assert!(params[1].ty.is_none());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_if_else() {
    let p = parse_ok("if x > 5 { 1 } else { 2 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::If { els: Some(_), .. }) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_else_if_chain() {
    let p = parse_ok("if a { 1 } else if b { 2 } else { 3 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::If { els: Some(e), .. }) => {
            assert!(matches!(**e, E::If { .. }));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_while() {
    let p = parse_ok("while x < 10 { f(x) }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::While { .. }) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_match() {
    let p = parse_ok("match x { .ok(v) => v, .err(e) => 0 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Match { arms, .. }) => assert_eq!(arms.len(), 2),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_match_multiline() {
    let p = parse_ok("match x {\n    .some(v) => v\n    .none => 0\n}");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Match { arms, .. }) => assert_eq!(arms.len(), 2),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_if_let() {
    let p = parse_ok("if let .some(x) = opt { x } else { 0 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::IfLet { pat, .. }) => {
            assert!(
                matches!(pat, zz_frontend::ast::Pattern::Variant { name, .. } if name == "some")
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_try() {
    let p = parse_ok("f()?");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Try { .. }) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_variant_constructors() {
    let p = parse_ok(".ok(1)");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Expr(E::Variant { name, arg, .. }) => {
            assert_eq!(name, "ok");
            assert!(arg.is_some());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_block_expression() {
    let p = parse_ok("x := { y := 1; y + 1 }");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { value, .. } => assert!(matches!(value, E::Block(_))),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_string_and_bool() {
    let p = parse_ok("s := \"hi\"\nb := true");
    match &p.stmts[0] {
        zz_frontend::ast::Stmt::Decl { value, .. } => assert!(matches!(value, E::Str { .. })),
        other => panic!("unexpected: {other:?}"),
    }
    match &p.stmts[1] {
        zz_frontend::ast::Stmt::Decl { value, .. } => assert!(matches!(value, E::Bool { .. })),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_dotted_path() {
    let prog = zz_frontend::parse("r := std.io.println(1)\n");
    assert!(prog.errors.is_empty(), "errors: {:?}", prog.errors);
    match &prog.program.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Call { callee, .. },
            ..
        } => match callee.as_ref() {
            E::Path { parts, .. } => {
                assert_eq!(parts, &["std", "io", "println"]);
            }
            other => panic!("expected Path callee, got {other:?}"),
        },
        other => panic!("expected Decl, got {other:?}"),
    }
}

#[test]
fn single_ident_is_not_path() {
    let prog = zz_frontend::parse("x := 1\n");
    assert!(prog.errors.is_empty());
    match &prog.program.stmts[0] {
        zz_frontend::ast::Stmt::Decl {
            value: E::Int { .. },
            ..
        } => {}
        other => panic!("expected plain decl, got {other:?}"),
    }
}

// --- string interpolation vs block-start ambiguity regression tests --------

#[test]
fn string_comparison_in_if_condition() {
    // `if x == "zaid" { ... }` must parse as a string comparison followed by
    // a block — NOT as a string interpolation.
    let p = parse_ok(
        r#"x := "a"
if x == "zaid" { println(x) }"#,
    );
    match &p.stmts[1] {
        zz_frontend::ast::Stmt::Expr(E::If { cond, .. }) => {
            // The condition should be a binary `==`, NOT an f-string.
            match cond.as_ref() {
                E::Binary { op: BinOp::Eq, .. } => {}
                other => panic!("expected ==, got {other:?}"),
            }
        }
        other => panic!("expected if stmt, got {other:?}"),
    }
}

#[test]
fn string_comparison_in_while_condition() {
    let p = parse_ok(
        r#"s := "test"
while s == "test" { println(s) }"#,
    );
    match &p.stmts[1] {
        zz_frontend::ast::Stmt::Expr(E::While { cond, .. }) => match cond.as_ref() {
            E::Binary { op: BinOp::Eq, .. } => {}
            other => panic!("expected ==, got {other:?}"),
        },
        other => panic!("expected while stmt, got {other:?}"),
    }
}

#[test]
fn string_interpolation_still_works() {
    // `"hello {name}"` must still parse as an f-string with interpolation.
    let p = parse_ok(
        r#"name := "world"
"hello {name}""#,
    );
    match &p.stmts[1] {
        zz_frontend::ast::Stmt::Expr(E::Fmt { parts, .. }) => {
            assert!(
                parts.len() >= 2,
                "expected text + interpolation, got {parts:?}"
            );
        }
        other => panic!("expected fmt expr, got {other:?}"),
    }
}

#[test]
fn string_interpolation_with_multiline_block_after() {
    // Multiline string comparison before a multiline block.
    let p = parse_ok(
        r#"x := "hello"
if x == "world" {
    println("yes")
}"#,
    );
    match &p.stmts[1] {
        zz_frontend::ast::Stmt::Expr(E::If { cond, .. }) => match cond.as_ref() {
            E::Binary { op: BinOp::Eq, .. } => {}
            other => panic!("expected ==, got {other:?}"),
        },
        other => panic!("expected if stmt, got {other:?}"),
    }
}
